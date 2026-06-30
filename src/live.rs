use crate::{
    config::OutputConfig,
    output::FrameOutput,
    playout::{Timeline, play_opened_input},
};
use anyhow::{Context, Result};
use ffmpeg_next::{Dictionary, format, frame};
use std::{
    sync::mpsc::{self, Receiver, SyncSender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

const LIVE_CHANNEL_CAPACITY: usize = 120;
const LIVE_IDLE_TIMEOUT: Duration = Duration::from_secs(3);

pub(crate) struct LiveReceiver {
    rx: Receiver<LiveEvent>,
    active: bool,
    video_pts: i64,
    audio_pts: i64,
}

enum LiveEvent {
    Started,
    Video(frame::Video),
    Audio(frame::Audio),
    Ended,
}

pub(crate) fn spawn_rtmp_listener(url: String, cfg: OutputConfig) -> LiveReceiver {
    let (tx, rx) = mpsc::sync_channel(LIVE_CHANNEL_CAPACITY);
    thread::spawn(move || run_rtmp_listener(url, cfg, tx));

    LiveReceiver {
        rx,
        active: false,
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
                Ok(LiveEvent::Started) => {
                    received_event = true;
                    eprintln!("live input connected; switching to RTMP live");
                    self.live.active = true;
                }
                Ok(LiveEvent::Video(mut frame)) => {
                    received_event = true;
                    if self.live.active {
                        frame.set_pts(Some(self.live.video_pts));
                        self.output.encode_video(&frame)?;
                        self.live.video_pts += 1;
                    }
                }
                Ok(LiveEvent::Audio(mut frame)) => {
                    received_event = true;
                    if self.live.active {
                        frame.set_pts(Some(self.live.audio_pts));
                        let samples = frame.samples() as i64;
                        self.output.encode_audio(&frame)?;
                        self.live.audio_pts += samples;
                    }
                }
                Ok(LiveEvent::Ended) => {
                    received_event = true;
                    eprintln!("live input ended; switching back to file playback");
                    self.live.active = false;
                }
                Err(TryRecvError::Empty) => return Ok(received_event),
                Err(TryRecvError::Disconnected) => {
                    self.live.active = false;
                    return Ok(received_event);
                }
            }
        }
    }

    fn wait_for_file_playback(&mut self) -> Result<()> {
        self.pump_live()?;
        let mut last_live_event = Instant::now();
        while self.live.active {
            thread::sleep(Duration::from_millis(10));
            if self.pump_live()? {
                last_live_event = Instant::now();
            } else if last_live_event.elapsed() >= LIVE_IDLE_TIMEOUT {
                eprintln!("live input idle; switching back to file playback");
                self.live.active = false;
            }
        }
        Ok(())
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
        let pts = frame
            .pts()
            .unwrap_or(self.live.video_pts)
            .max(self.live.video_pts);
        frame.set_pts(Some(pts));
        self.output.encode_video(&frame)?;
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
        let pts = frame
            .pts()
            .unwrap_or(self.live.audio_pts)
            .max(self.live.audio_pts);
        frame.set_pts(Some(pts));
        self.output.encode_audio(&frame)?;
        self.live.audio_pts = pts + samples;
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
    tx: SyncSender<LiveEvent>,
}

impl FrameOutput for LiveFrameSender {
    fn audio_frame_size(&self) -> usize {
        1024
    }

    fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        self.tx
            .send(LiveEvent::Video(frame.clone()))
            .context("failed to send live video frame")
    }

    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        self.tx
            .send(LiveEvent::Audio(frame.clone()))
            .context("failed to send live audio frame")
    }
}

fn run_rtmp_listener(url: String, cfg: OutputConfig, tx: SyncSender<LiveEvent>) {
    loop {
        match open_rtmp_listener(&url) {
            Ok(ictx) => {
                if tx.send(LiveEvent::Started).is_err() {
                    return;
                }

                let mut timeline = Timeline::new();
                let mut output = LiveFrameSender { tx: tx.clone() };
                if let Err(error) =
                    play_opened_input(&url, ictx, &cfg, &mut timeline, &mut output, None, None)
                {
                    eprintln!("live input failed: {error:#}");
                }

                if tx.send(LiveEvent::Ended).is_err() {
                    return;
                }
            }
            Err(error) => {
                eprintln!("RTMP listener failed: {error:#}; retrying");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn open_rtmp_listener(url: &str) -> Result<format::context::Input> {
    let mut options = Dictionary::new();
    options.set("listen", "1");
    options.set("rw_timeout", "3000000");
    format::input_with_dictionary(url, options)
        .with_context(|| format!("failed to listen for RTMP input at {url}"))
}
