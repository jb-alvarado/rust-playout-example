use crate::{
    config::OutputConfig,
    output::FrameOutput,
    playout::{Timeline, play_opened_input},
};
use anyhow::{Context, Result};
use ffmpeg_next::{
    Dictionary, Error as FfmpegError, ffi, format, frame,
    util::{
        channel_layout::ChannelLayout,
        format::sample::{Sample, Type as SampleType},
        interrupt,
    },
};
use log::{error, info};
use std::{
    ffi::CString,
    ptr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, TryRecvError},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const LIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const LIVE_WATCHDOG_INTERVAL: Duration = Duration::from_millis(100);

pub(crate) struct LiveReceiver {
    rx: Receiver<LiveEvent>,
    fps: u32,
    sample_rate: u32,
    active: bool,
    session_id: u64,
    session_video_base: Option<i64>,
    session_audio_base: Option<i64>,
    last_media_at: Option<Instant>,
    last_video_frame: Option<frame::Video>,
    last_video_output_pts: Option<i64>,
    last_audio_output_end_pts: Option<i64>,
    file_resume_at_seconds: Option<f64>,
    file_resume_shift_seconds: Option<f64>,
    video_pts: i64,
    audio_pts: i64,
}

enum LiveEvent {
    Started(u64),
    Video(u64, frame::Video),
    Audio(u64, frame::Audio),
    Ended(u64),
}

pub(crate) fn spawn_rtmp_listener(url: String, cfg: OutputConfig) -> LiveReceiver {
    let fps = cfg.fps;
    let sample_rate = cfg.sample_rate;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || run_rtmp_listener(url, cfg, tx));

    LiveReceiver {
        rx,
        fps,
        sample_rate,
        active: false,
        session_id: 0,
        session_video_base: None,
        session_audio_base: None,
        last_media_at: None,
        last_video_frame: None,
        last_video_output_pts: None,
        last_audio_output_end_pts: None,
        file_resume_at_seconds: None,
        file_resume_shift_seconds: None,
        video_pts: 0,
        audio_pts: 0,
    }
}

pub(crate) struct LiveOverrideOutput<'a, O: FrameOutput> {
    output: &'a mut O,
    live: &'a mut LiveReceiver,
}

impl<'a, O: FrameOutput> LiveOverrideOutput<'a, O> {
    pub(crate) fn new(output: &'a mut O, live: &'a mut LiveReceiver) -> Self {
        Self { output, live }
    }

    fn pump_live(&mut self) -> Result<bool> {
        let mut received_event = false;
        loop {
            match self.live.rx.try_recv() {
                Ok(LiveEvent::Started(session_id)) => {
                    self.live.session_id = session_id;
                    self.live.session_video_base = None;
                    self.live.session_audio_base = None;
                    self.live.last_media_at = Some(Instant::now());
                    info!("live input connected; waiting for first video frame");
                }
                Ok(LiveEvent::Video(session_id, mut frame)) => {
                    if session_id == self.live.session_id {
                        if !self.live.active {
                            info!("first live video frame received; switching to RTMP live");
                            self.live.active = true;
                        }
                        received_event = true;
                        let source_pts = frame.pts().unwrap_or(0);
                        let session_video_base = *self
                            .live
                            .session_video_base
                            .get_or_insert(self.live.video_pts - source_pts);
                        let pts = (session_video_base + source_pts).max(self.live.video_pts);
                        self.fill_video_until(pts)?;
                        frame.set_pts(Some(pts));
                        self.output.encode_video(&frame)?;
                        self.remember_video_frame(frame, pts);
                        self.live.last_media_at = Some(Instant::now());
                        self.live.video_pts = pts + 1;
                    }
                }
                Ok(LiveEvent::Audio(session_id, mut frame)) => {
                    if self.live.active && session_id == self.live.session_id {
                        received_event = true;
                        let samples = frame.samples() as i64;
                        let source_pts = frame.pts().unwrap_or(0);
                        let session_audio_base = *self
                            .live
                            .session_audio_base
                            .get_or_insert(self.live.audio_pts - source_pts);
                        let pts = (session_audio_base + source_pts).max(self.live.audio_pts);
                        self.fill_audio_until(pts)?;
                        frame.set_pts(Some(pts));
                        self.output.encode_audio(&frame)?;
                        self.live.last_media_at = Some(Instant::now());
                        self.remember_audio_frame_end(pts + samples);
                    }
                }
                Ok(LiveEvent::Ended(session_id)) => {
                    if session_id == self.live.session_id {
                        info!("live input ended; switching back to file playback");
                        if self.live.active {
                            self.fill_live_gap_since_last_media()?;
                            self.align_live_pts_to_common_time();
                            self.prepare_file_resume();
                        }
                        self.live.active = false;
                    }
                }
                Err(TryRecvError::Empty) => return Ok(received_event),
                Err(TryRecvError::Disconnected) => {
                    self.fill_live_gap_since_last_media()?;
                    self.align_live_pts_to_common_time();
                    self.prepare_file_resume();
                    self.live.active = false;
                    return Ok(received_event);
                }
            }
        }
    }

    fn wait_for_file_playback(&mut self) -> Result<()> {
        self.pump_live()?;
        while self.live.active {
            thread::sleep(Duration::from_millis(10));
            self.pump_live()?;
            let idle_for = self
                .live
                .last_media_at
                .map(|last_media_at| last_media_at.elapsed())
                .unwrap_or_default();
            if idle_for >= LIVE_IDLE_TIMEOUT {
                info!("live input idle; switching back to file playback");
                self.fill_live_gap(idle_for)?;
                self.align_live_pts_to_common_time();
                self.prepare_file_resume();
                self.live.active = false;
            }
        }
        Ok(())
    }

    fn fill_live_gap_since_last_media(&mut self) -> Result<()> {
        if let Some(last_media_at) = self.live.last_media_at {
            self.fill_live_gap(last_media_at.elapsed())?;
        }
        Ok(())
    }

    fn fill_live_gap(&mut self, duration: Duration) -> Result<()> {
        let video_frames = (duration.as_secs_f64() * f64::from(self.live.fps)).ceil() as i64;
        if let Some(last_video_frame) = self.live.last_video_frame.clone() {
            for _ in 0..video_frames {
                let mut frame = last_video_frame.clone();
                frame.set_pts(Some(self.live.video_pts));
                self.output.encode_video(&frame)?;
                self.remember_video_frame(frame, self.live.video_pts);
                self.live.video_pts += 1;
            }
        } else {
            self.live.video_pts += video_frames;
        }

        let mut remaining_samples =
            (duration.as_secs_f64() * f64::from(self.live.sample_rate)).ceil() as usize;
        let frame_size = self.output.audio_frame_size().max(1);
        while remaining_samples > 0 {
            let samples = remaining_samples.min(frame_size);
            let mut frame = frame::Audio::new(
                Sample::F32(SampleType::Planar),
                samples,
                ChannelLayout::STEREO,
            );
            frame.set_rate(self.live.sample_rate);
            frame.set_pts(Some(self.live.audio_pts));
            for channel in 0..2 {
                for sample in frame.plane_mut::<f32>(channel) {
                    *sample = 0.0;
                }
            }
            self.output.encode_audio(&frame)?;
            self.remember_audio_frame_end(self.live.audio_pts + samples as i64);
            remaining_samples -= samples;
        }

        self.live.last_media_at = Some(Instant::now());
        Ok(())
    }

    fn fill_video_until(&mut self, next_pts: i64) -> Result<()> {
        let Some(mut fill_pts) = self.live.last_video_output_pts.map(|pts| pts + 1) else {
            return Ok(());
        };
        let Some(last_video_frame) = self.live.last_video_frame.clone() else {
            return Ok(());
        };

        while fill_pts < next_pts {
            let mut frame = last_video_frame.clone();
            frame.set_pts(Some(fill_pts));
            self.output.encode_video(&frame)?;
            self.remember_video_frame(frame, fill_pts);
            fill_pts += 1;
        }

        Ok(())
    }

    fn fill_audio_until(&mut self, next_pts: i64) -> Result<()> {
        let Some(mut fill_pts) = self.live.last_audio_output_end_pts else {
            return Ok(());
        };
        if fill_pts >= next_pts {
            return Ok(());
        }

        let frame_size = self.output.audio_frame_size().max(1);
        while fill_pts < next_pts {
            let samples = (next_pts - fill_pts).min(frame_size as i64) as usize;
            let mut frame = frame::Audio::new(
                Sample::F32(SampleType::Planar),
                samples,
                ChannelLayout::STEREO,
            );
            frame.set_rate(self.live.sample_rate);
            frame.set_pts(Some(fill_pts));
            for channel in 0..2 {
                for sample in frame.plane_mut::<f32>(channel) {
                    *sample = 0.0;
                }
            }
            self.output.encode_audio(&frame)?;
            fill_pts += samples as i64;
            self.remember_audio_frame_end(fill_pts);
        }

        Ok(())
    }

    fn remember_video_frame(&mut self, frame: frame::Video, pts: i64) {
        self.live.last_video_frame = Some(frame);
        self.live.last_video_output_pts = Some(pts);
    }

    fn remember_audio_frame_end(&mut self, end_pts: i64) {
        self.live.audio_pts = end_pts;
        self.live.last_audio_output_end_pts = Some(end_pts);
    }

    fn align_live_pts_to_common_time(&mut self) {
        let video_seconds = self.live.video_pts as f64 / f64::from(self.live.fps);
        let audio_seconds = self.live.audio_pts as f64 / f64::from(self.live.sample_rate);
        let common_seconds = video_seconds.max(audio_seconds);
        self.live.video_pts = self
            .live
            .video_pts
            .max((common_seconds * f64::from(self.live.fps)).ceil() as i64);
        self.live.audio_pts = self
            .live
            .audio_pts
            .max((common_seconds * f64::from(self.live.sample_rate)).ceil() as i64);
    }

    fn prepare_file_resume(&mut self) {
        let video_seconds = self.live.video_pts as f64 / f64::from(self.live.fps);
        let audio_seconds = self.live.audio_pts as f64 / f64::from(self.live.sample_rate);
        self.live.file_resume_at_seconds = Some(video_seconds.max(audio_seconds));
        self.live.file_resume_shift_seconds = None;
    }

    fn file_video_pts(&mut self, source_pts: i64) -> i64 {
        if let Some(resume_seconds) = self.live.file_resume_at_seconds {
            let source_seconds = source_pts as f64 / f64::from(self.live.fps);
            let shift_seconds = *self
                .live
                .file_resume_shift_seconds
                .get_or_insert(resume_seconds - source_seconds);
            ((source_seconds + shift_seconds) * f64::from(self.live.fps)).round() as i64
        } else {
            source_pts.max(self.live.video_pts)
        }
        .max(self.live.video_pts)
    }

    fn file_audio_pts(&mut self, source_pts: i64) -> i64 {
        if let Some(resume_seconds) = self.live.file_resume_at_seconds {
            let source_seconds = source_pts as f64 / f64::from(self.live.sample_rate);
            let shift_seconds = *self
                .live
                .file_resume_shift_seconds
                .get_or_insert(resume_seconds - source_seconds);
            ((source_seconds + shift_seconds) * f64::from(self.live.sample_rate)).round() as i64
        } else {
            source_pts.max(self.live.audio_pts)
        }
        .max(self.live.audio_pts)
    }
}

impl<O: FrameOutput> FrameOutput for LiveOverrideOutput<'_, O> {
    fn audio_frame_size(&self) -> usize {
        self.output.audio_frame_size()
    }

    fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        if !self.live.active
            && let Some(pts) = frame.pts()
        {
            self.live.video_pts = self.live.video_pts.max(pts);
        }
        self.wait_for_file_playback()?;

        let mut frame = frame.clone();
        let pts = self.file_video_pts(frame.pts().unwrap_or(self.live.video_pts));
        self.fill_video_until(pts)?;
        frame.set_pts(Some(pts));
        self.output.encode_video(&frame)?;
        self.remember_video_frame(frame, pts);
        self.live.video_pts = pts + 1;
        Ok(())
    }

    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        if !self.live.active
            && let Some(pts) = frame.pts()
        {
            self.live.audio_pts = self.live.audio_pts.max(pts);
        }
        self.wait_for_file_playback()?;

        let mut frame = frame.clone();
        let samples = frame.samples() as i64;
        let pts = self.file_audio_pts(frame.pts().unwrap_or(self.live.audio_pts));
        self.fill_audio_until(pts)?;
        frame.set_pts(Some(pts));
        self.output.encode_audio(&frame)?;
        self.remember_audio_frame_end(pts + samples);
        Ok(())
    }

    fn set_video_end(&mut self, video_end_pts: Option<i64>) -> Result<()> {
        self.output.set_video_end(video_end_pts)
    }

    fn video_finished(&mut self) -> Result<()> {
        self.output.video_finished()
    }

    fn write_vtt_subtitles(
        &mut self,
        media_path: &str,
        output_start_ms: i64,
        source_start_ms: i64,
    ) -> Result<()> {
        self.output
            .write_vtt_subtitles(media_path, output_start_ms, source_start_ms)
    }
}

struct LiveFrameSender {
    tx: Sender<LiveEvent>,
    session_id: u64,
    last_frame_ms: Arc<AtomicU64>,
}

impl FrameOutput for LiveFrameSender {
    fn audio_frame_size(&self) -> usize {
        1024
    }

    fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        self.last_frame_ms.store(now_millis(), Ordering::Relaxed);
        self.tx
            .send(LiveEvent::Video(self.session_id, frame.clone()))
            .context("failed to send live video frame")
    }

    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        self.last_frame_ms.store(now_millis(), Ordering::Relaxed);
        self.tx
            .send(LiveEvent::Audio(self.session_id, frame.clone()))
            .context("failed to send live audio frame")
    }
}

fn run_rtmp_listener(url: String, cfg: OutputConfig, tx: Sender<LiveEvent>) {
    let mut session_id = 0;

    loop {
        let abort = Arc::new(AtomicBool::new(false));

        match open_rtmp_listener(&url, Arc::clone(&abort)) {
            Ok(ictx) => {
                session_id += 1;
                let last_frame_ms = Arc::new(AtomicU64::new(now_millis()));
                let watchdog = spawn_live_watchdog(Arc::clone(&last_frame_ms), Arc::clone(&abort));

                if tx.send(LiveEvent::Started(session_id)).is_err() {
                    abort.store(true, Ordering::Relaxed);
                    let _ = watchdog.join();
                    return;
                }

                let (done_tx, done_rx) = mpsc::sync_channel(1);
                let mut output = LiveFrameSender {
                    tx: tx.clone(),
                    session_id,
                    last_frame_ms,
                };

                let worker_url = url.clone();
                let worker_cfg = cfg.clone();
                let worker = thread::spawn(move || {
                    let mut timeline = Timeline::new();
                    let result = play_opened_input(
                        &worker_url,
                        ictx,
                        &worker_cfg,
                        &mut timeline,
                        &mut output,
                        None,
                        None,
                    );
                    let _ = done_tx.send(result.map_err(|error| format!("{error:#}")));
                });

                let mut worker_finished = false;
                while !abort.load(Ordering::Relaxed) {
                    if let Ok(result) = done_rx.try_recv() {
                        worker_finished = true;
                        if let Err(error) = result {
                            error!("live input failed: {error}");
                        }
                        break;
                    }
                    thread::sleep(Duration::from_millis(10));
                }

                abort.store(true, Ordering::Relaxed);
                let _ = watchdog.join();
                if worker_finished {
                    let _ = worker.join();
                } else {
                    info!(
                        "live input reader is still blocked; restarting ingest server without waiting"
                    );
                }

                info!("Restart ingest server after live input ended");
                if tx.send(LiveEvent::Ended(session_id)).is_err() {
                    return;
                }
            }
            Err(error) => {
                abort.store(true, Ordering::Relaxed);
                error!("RTMP listener failed: {error:#}; retrying");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn spawn_live_watchdog(
    last_frame_ms: Arc<AtomicU64>,
    abort: Arc<AtomicBool>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        while !abort.load(Ordering::Relaxed) {
            thread::sleep(LIVE_WATCHDOG_INTERVAL);

            let last_frame_ms = last_frame_ms.load(Ordering::Relaxed);
            if now_millis().saturating_sub(last_frame_ms) >= LIVE_IDLE_TIMEOUT.as_millis() as u64 {
                info!("live input disconnected or idle; restarting ingest server");
                abort.store(true, Ordering::Relaxed);
                return;
            }
        }
    })
}

fn open_rtmp_listener(url: &str, abort: Arc<AtomicBool>) -> Result<format::context::Input> {
    info!("Start ingest server, listen on: {url}");

    let mut options = Dictionary::new();
    options.set("listen", "1");
    options.set("timeout", "0");

    let url_cstring =
        CString::new(url).with_context(|| format!("RTMP input URL contains NUL byte: {url}"))?;
    let interrupt_abort = Arc::clone(&abort);
    let interrupt = interrupt::new(Box::new(move || interrupt_abort.load(Ordering::Relaxed)));

    unsafe {
        let mut ps = ffi::avformat_alloc_context();
        if ps.is_null() {
            anyhow::bail!("failed to allocate RTMP input context");
        }

        (*ps).interrupt_callback = interrupt.interrupt;

        let mut opts = options.disown();
        let open_result =
            ffi::avformat_open_input(&mut ps, url_cstring.as_ptr(), ptr::null_mut(), &mut opts);
        Dictionary::own(opts);

        if open_result != 0 {
            ffi::avformat_close_input(&mut ps);
            return Err(FfmpegError::from(open_result))
                .with_context(|| format!("failed to listen for RTMP input at {url}"));
        }

        let stream_info_result = ffi::avformat_find_stream_info(ps, ptr::null_mut());
        if stream_info_result < 0 {
            ffi::avformat_close_input(&mut ps);
            return Err(FfmpegError::from(stream_info_result))
                .with_context(|| format!("failed to read RTMP stream info at {url}"));
        }

        Ok(format::context::Input::wrap(ps))
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
