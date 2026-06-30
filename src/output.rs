use crate::{clock::PlayoutClock, config::OutputConfig};
use anyhow::{Context, Result, anyhow};
use ffmpeg::{
    codec, format, frame,
    util::{
        channel_layout::ChannelLayout, format::pixel::Pixel, format::sample::Sample,
        rational::Rational,
    },
};
use ffmpeg_next as ffmpeg;
use std::{collections::VecDeque, error::Error, fmt};

#[derive(Debug)]
pub(crate) struct PlaybackStopped;

impl fmt::Display for PlaybackStopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("desktop playback stopped by user")
    }
}

impl Error for PlaybackStopped {}

pub(crate) trait FrameOutput {
    fn audio_frame_size(&self) -> usize;
    fn encode_video(&mut self, frame: &frame::Video) -> Result<()>;
    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()>;
    fn set_video_end(&mut self, _video_end_pts: Option<i64>) -> Result<()> {
        Ok(())
    }
    fn video_finished(&mut self) -> Result<()> {
        Ok(())
    }
}

pub(crate) struct Output {
    kind: OutputKind,
}

enum OutputKind {
    Encoded(EncodedOutput),
    #[cfg(feature = "desktop")]
    Desktop(desktop::DesktopOutput),
}

struct EncodedOutput {
    octx: format::context::Output,
    video_stream_index: usize,
    audio_stream_index: usize,
    video_encoder: codec::encoder::video::Encoder,
    audio_encoder: codec::encoder::audio::Encoder,
    audio_buffer: [VecDeque<f32>; 2],
    audio_buffer_pts: Option<i64>,
    audio_sample_rate: u32,
    clock: PlayoutClock,
}

impl Output {
    pub(crate) fn open(path: &str, cfg: &OutputConfig) -> Result<Self> {
        Ok(Self {
            kind: OutputKind::Encoded(EncodedOutput::open(path, cfg)?),
        })
    }

    #[cfg(feature = "desktop")]
    pub(crate) fn open_desktop(cfg: &OutputConfig) -> Result<Self> {
        Ok(Self {
            kind: OutputKind::Desktop(desktop::DesktopOutput::open(cfg)?),
        })
    }

    pub(crate) fn audio_frame_size(&self) -> usize {
        match &self.kind {
            OutputKind::Encoded(output) => output.audio_frame_size(),
            #[cfg(feature = "desktop")]
            OutputKind::Desktop(output) => output.audio_frame_size(),
        }
    }

    pub(crate) fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        match &mut self.kind {
            OutputKind::Encoded(output) => output.encode_video(frame),
            #[cfg(feature = "desktop")]
            OutputKind::Desktop(output) => output.encode_video(frame),
        }
    }

    pub(crate) fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        match &mut self.kind {
            OutputKind::Encoded(output) => output.encode_audio(frame),
            #[cfg(feature = "desktop")]
            OutputKind::Desktop(output) => output.encode_audio(frame),
        }
    }

    pub(crate) fn finish(self) -> Result<()> {
        match self.kind {
            OutputKind::Encoded(output) => output.finish(),
            #[cfg(feature = "desktop")]
            OutputKind::Desktop(output) => output.finish(),
        }
    }

    #[cfg(feature = "desktop")]
    pub(crate) fn is_desktop(&self) -> bool {
        matches!(self.kind, OutputKind::Desktop(_))
    }

    #[cfg(feature = "desktop")]
    pub(crate) fn run_desktop<T, F>(&mut self, operation: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&mut desktop::DesktopFrameSender) -> T + Send + 'static,
    {
        match &mut self.kind {
            OutputKind::Desktop(output) => output.run_operation(operation),
            OutputKind::Encoded(_) => Err(anyhow!("output is not in desktop mode")),
        }
    }
}

impl FrameOutput for Output {
    fn audio_frame_size(&self) -> usize {
        Self::audio_frame_size(self)
    }

    fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        Self::encode_video(self, frame)
    }

    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        Self::encode_audio(self, frame)
    }
}

impl EncodedOutput {
    fn open(path: &str, cfg: &OutputConfig) -> Result<Self> {
        let mut octx = if path.starts_with("rtmp://") || path.starts_with("rtmps://") {
            format::output_as(path, "flv")?
        } else {
            format::output(path)?
        };

        let global_header = octx
            .format()
            .flags()
            .contains(format::flag::Flags::GLOBAL_HEADER);

        let video_codec =
            codec::encoder::find(codec::Id::H264).context("H.264 encoder not found")?;
        let mut video_stream = octx.add_stream(video_codec)?;
        let mut video_ctx = codec::context::Context::new_with_codec(video_codec)
            .encoder()
            .video()?;
        video_ctx.set_width(cfg.width);
        video_ctx.set_height(cfg.height);
        video_ctx.set_format(Pixel::YUV420P);
        video_ctx.set_time_base(cfg.video_time_base);
        video_ctx.set_frame_rate(Some(Rational(cfg.fps as i32, 1)));
        video_ctx.set_bit_rate(3_000_000);
        if global_header {
            video_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
        }
        let video_encoder = video_ctx.open_as(video_codec)?;
        video_stream.set_parameters(&video_encoder);
        video_stream.set_time_base(cfg.video_time_base);
        let video_stream_index = video_stream.index();

        let audio_codec = codec::encoder::find(codec::Id::AAC).context("AAC encoder not found")?;
        let mut audio_stream = octx.add_stream(audio_codec)?;
        let mut audio_ctx = codec::context::Context::new_with_codec(audio_codec)
            .encoder()
            .audio()?;
        audio_ctx.set_rate(cfg.sample_rate as i32);
        audio_ctx.set_channel_layout(ChannelLayout::STEREO);
        audio_ctx.set_format(Sample::F32(ffmpeg::format::sample::Type::Planar));
        audio_ctx.set_time_base(cfg.audio_time_base);
        audio_ctx.set_bit_rate(128_000);
        if global_header {
            audio_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
        }
        let audio_encoder = audio_ctx.open_as(audio_codec)?;
        audio_stream.set_parameters(&audio_encoder);
        audio_stream.set_time_base(cfg.audio_time_base);
        let audio_stream_index = audio_stream.index();

        octx.write_header()?;

        Ok(Self {
            octx,
            video_stream_index,
            audio_stream_index,
            video_encoder,
            audio_encoder,
            audio_buffer: [VecDeque::new(), VecDeque::new()],
            audio_buffer_pts: None,
            audio_sample_rate: cfg.sample_rate,
            clock: PlayoutClock::new(),
        })
    }

    fn audio_frame_size(&self) -> usize {
        self.audio_encoder.frame_size() as usize
    }

    fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
        self.video_encoder.send_frame(frame)?;
        let mut packet = ffmpeg::Packet::empty();
        while self.video_encoder.receive_packet(&mut packet).is_ok() {
            self.write_packet(
                &mut packet,
                self.video_stream_index,
                self.video_encoder.time_base(),
            )?;
        }
        Ok(())
    }

    fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
        if frame.samples() == 0 {
            return Ok(());
        }

        if self.audio_buffer[0].is_empty() {
            self.audio_buffer_pts = frame.pts();
        }
        for channel in 0..self.audio_buffer.len() {
            self.audio_buffer[channel].extend(
                frame
                    .plane::<f32>(channel)
                    .iter()
                    .map(|sample| if sample.is_finite() { *sample } else { 0.0 }),
            );
        }

        self.write_complete_audio_frames()
    }

    fn write_complete_audio_frames(&mut self) -> Result<()> {
        let frame_size = self.audio_frame_size();
        if frame_size == 0 {
            return Err(anyhow!("audio encoder reported a frame size of zero"));
        }

        while self.audio_buffer[0].len() >= frame_size {
            let mut frame = frame::Audio::new(
                Sample::F32(ffmpeg::format::sample::Type::Planar),
                frame_size,
                ChannelLayout::STEREO,
            );
            frame.set_rate(self.audio_sample_rate);
            frame.set_pts(self.audio_buffer_pts);

            for channel in 0..self.audio_buffer.len() {
                for sample in frame.plane_mut::<f32>(channel) {
                    *sample = self.audio_buffer[channel]
                        .pop_front()
                        .context("audio buffer is unexpectedly incomplete")?;
                }
            }

            self.audio_buffer_pts = self
                .audio_buffer_pts
                .map(|pts| pts + self.audio_frame_size() as i64);
            self.send_audio_frame(&frame)?;
        }

        Ok(())
    }

    fn send_audio_frame(&mut self, frame: &frame::Audio) -> Result<()> {
        self.audio_encoder.send_frame(frame)?;
        let mut packet = ffmpeg::Packet::empty();
        while self.audio_encoder.receive_packet(&mut packet).is_ok() {
            self.write_packet(
                &mut packet,
                self.audio_stream_index,
                self.audio_encoder.time_base(),
            )?;
        }
        Ok(())
    }

    fn pad_audio_buffer(&mut self) -> Result<()> {
        if self.audio_buffer[0].is_empty() {
            return Ok(());
        }

        let frame_size = self.audio_frame_size();
        for channel in &mut self.audio_buffer {
            channel.resize(frame_size, 0.0);
        }
        self.write_complete_audio_frames()
    }

    fn write_packet(
        &mut self,
        packet: &mut ffmpeg::Packet,
        stream_index: usize,
        encoder_time_base: Rational,
    ) -> Result<()> {
        let stream_time_base = self
            .octx
            .stream(stream_index)
            .context("output stream is missing")?
            .time_base();

        packet.set_stream(stream_index);
        packet.rescale_ts(encoder_time_base, stream_time_base);
        self.clock
            .wait_until(packet.dts().or_else(|| packet.pts()), stream_time_base);
        packet.write_interleaved(&mut self.octx)?;
        Ok(())
    }

    fn finish(mut self) -> Result<()> {
        self.video_encoder.send_eof()?;
        let mut packet = ffmpeg::Packet::empty();
        while self.video_encoder.receive_packet(&mut packet).is_ok() {
            self.write_packet(
                &mut packet,
                self.video_stream_index,
                self.video_encoder.time_base(),
            )?;
        }

        self.pad_audio_buffer()?;
        self.audio_encoder.send_eof()?;
        let mut packet = ffmpeg::Packet::empty();
        while self.audio_encoder.receive_packet(&mut packet).is_ok() {
            self.write_packet(
                &mut packet,
                self.audio_stream_index,
                self.audio_encoder.time_base(),
            )?;
        }

        self.octx.write_trailer()?;
        Ok(())
    }
}

#[cfg(feature = "desktop")]
mod desktop {
    use super::*;
    use ffmpeg_next::Rescale;
    use sdl2::{
        Sdl,
        audio::{AudioQueue, AudioSpecDesired},
        event::Event,
        keyboard::Keycode,
        pixels::{Color, PixelFormatEnum},
        render::{Canvas, Texture},
        video::Window,
    };
    use std::{
        collections::VecDeque,
        mem::size_of,
        sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, sync_channel},
        thread,
        time::{Duration, Instant},
    };

    const AUDIO_CHANNELS: usize = 2;
    const AUDIO_PREBUFFER_MS: u64 = 100;
    const AUDIO_MAX_QUEUE_MS: u64 = 250;
    const VIDEO_STARVATION_GRACE_FRAMES: i64 = 2;
    const SCHEDULER_INTERVAL: Duration = Duration::from_millis(2);
    const OUTPUT_CHANNEL_CAPACITY: usize = 64;

    pub(super) struct DesktopOutput {
        renderer: DesktopRenderer,
    }

    enum DesktopMessage {
        ClipStarted,
        Video(frame::Video),
        Audio {
            samples: Vec<f32>,
            samples_per_channel: usize,
        },
        VideoEnd(Option<i64>),
        VideoFinished,
        ClipFinished,
    }

    pub(crate) struct DesktopFrameSender {
        sender: SyncSender<DesktopMessage>,
    }

    struct DesktopRenderer {
        // Texture must be dropped before its parent canvas.
        texture: Texture,
        canvas: Canvas<Window>,
        audio: AudioQueue<f32>,
        event_pump: sdl2::EventPump,
        video_queue: VecDeque<frame::Video>,
        submitted_audio_samples: u64,
        audio_started: bool,
        sample_rate: u32,
        device_buffer_samples: u64,
        audio_clock: AudioMasterClock,
        video_time_base: Rational,
        video_end_pts: Option<i64>,
        video_finished: bool,
        last_rendered_video_pts: Option<i64>,
        last_starvation_report: Option<Instant>,
        // SDL context must outlive all resources above.
        _sdl: Sdl,
    }

    impl DesktopOutput {
        pub(super) fn open(cfg: &OutputConfig) -> Result<Self> {
            Ok(Self {
                renderer: DesktopRenderer::open(cfg)?,
            })
        }

        pub(super) fn audio_frame_size(&self) -> usize {
            1024
        }

        pub(super) fn encode_video(&mut self, _frame: &frame::Video) -> Result<()> {
            Err(anyhow!(
                "desktop frames must be produced by the decode worker"
            ))
        }

        pub(super) fn encode_audio(&mut self, _frame: &frame::Audio) -> Result<()> {
            Err(anyhow!(
                "desktop audio must be produced by the decode worker"
            ))
        }

        pub(super) fn run_operation<T, F>(&mut self, operation: F) -> Result<T>
        where
            T: Send + 'static,
            F: FnOnce(&mut DesktopFrameSender) -> T + Send + 'static,
        {
            let (sender, receiver) = sync_channel(OUTPUT_CHANNEL_CAPACITY);
            let worker = thread::Builder::new()
                .name("ffplayout-decode".to_string())
                .spawn(move || {
                    let mut output = DesktopFrameSender { sender };
                    let _ = output.sender.send(DesktopMessage::ClipStarted);
                    let result = operation(&mut output);
                    let _ = output.sender.send(DesktopMessage::ClipFinished);
                    result
                })
                .map_err(|error| anyhow!("failed to start decode worker: {error}"))?;

            let render_result = self.renderer.run_clip(receiver);
            let worker_result = worker
                .join()
                .map_err(|_| anyhow!("decode worker panicked"))?;
            render_result?;
            Ok(worker_result)
        }

        pub(super) fn finish(self) -> Result<()> {
            self.renderer.finish()
        }
    }

    impl FrameOutput for DesktopFrameSender {
        fn audio_frame_size(&self) -> usize {
            1024
        }

        fn encode_video(&mut self, frame: &frame::Video) -> Result<()> {
            self.sender
                .send(DesktopMessage::Video(frame.clone()))
                .map_err(|_| PlaybackStopped.into())
        }

        fn encode_audio(&mut self, frame: &frame::Audio) -> Result<()> {
            if frame.samples() == 0 {
                return Ok(());
            }

            let left = frame.plane::<f32>(0);
            let right = frame.plane::<f32>(1);
            let mut interleaved = Vec::with_capacity(frame.samples() * AUDIO_CHANNELS);
            for (left, right) in left.iter().zip(right) {
                interleaved.push(if left.is_finite() { *left } else { 0.0 });
                interleaved.push(if right.is_finite() { *right } else { 0.0 });
            }
            self.sender
                .send(DesktopMessage::Audio {
                    samples: interleaved,
                    samples_per_channel: frame.samples(),
                })
                .map_err(|_| PlaybackStopped.into())
        }

        fn set_video_end(&mut self, video_end_pts: Option<i64>) -> Result<()> {
            self.sender
                .send(DesktopMessage::VideoEnd(video_end_pts))
                .map_err(|_| PlaybackStopped.into())
        }

        fn video_finished(&mut self) -> Result<()> {
            self.sender
                .send(DesktopMessage::VideoFinished)
                .map_err(|_| PlaybackStopped.into())
        }
    }

    impl DesktopRenderer {
        fn open(cfg: &OutputConfig) -> Result<Self> {
            let sdl = sdl2::init().map_err(anyhow::Error::msg)?;
            let video = sdl.video().map_err(anyhow::Error::msg)?;
            let audio_subsystem = sdl.audio().map_err(anyhow::Error::msg)?;
            let window = video
                .window("ffplayout", cfg.width, cfg.height)
                .position_centered()
                .resizable()
                .build()
                .map_err(|error| anyhow!("{error}"))?;
            let canvas = window
                .into_canvas()
                .accelerated()
                .build()
                .map_err(|error| anyhow!("{error}"))?;
            let texture = canvas
                .texture_creator()
                .create_texture_streaming(PixelFormatEnum::IYUV, cfg.width, cfg.height)
                .map_err(|error| anyhow!("{error}"))?;
            let desired = AudioSpecDesired {
                freq: Some(cfg.sample_rate as i32),
                channels: Some(2),
                samples: Some(1024),
            };
            let audio = audio_subsystem
                .open_queue::<f32, _>(None, &desired)
                .map_err(anyhow::Error::msg)?;
            if audio.spec().freq != cfg.sample_rate as i32
                || usize::from(audio.spec().channels) != AUDIO_CHANNELS
            {
                return Err(anyhow!(
                    "SDL opened an incompatible audio format: {} Hz, {} channel(s)",
                    audio.spec().freq,
                    audio.spec().channels
                ));
            }
            let device_buffer_samples = u64::from(audio.spec().samples);
            let event_pump = sdl.event_pump().map_err(anyhow::Error::msg)?;

            Ok(Self {
                canvas,
                audio,
                event_pump,
                video_queue: VecDeque::new(),
                submitted_audio_samples: 0,
                audio_started: false,
                sample_rate: cfg.sample_rate,
                device_buffer_samples,
                audio_clock: AudioMasterClock::new(cfg.sample_rate, device_buffer_samples),
                video_time_base: cfg.video_time_base,
                video_end_pts: None,
                video_finished: false,
                last_rendered_video_pts: None,
                last_starvation_report: None,
                texture,
                _sdl: sdl,
            })
        }

        fn run_clip(&mut self, receiver: Receiver<DesktopMessage>) -> Result<()> {
            loop {
                self.handle_events()?;
                self.render_due_video()?;

                if self.audio_started && self.queued_audio_samples() > self.max_queue_samples() {
                    thread::sleep(SCHEDULER_INTERVAL);
                    continue;
                }

                match receiver.recv_timeout(SCHEDULER_INTERVAL) {
                    Ok(DesktopMessage::ClipStarted) => {
                        self.video_end_pts = None;
                        self.video_finished = false;
                        self.last_starvation_report = None;
                    }
                    Ok(DesktopMessage::Video(frame)) => self.video_queue.push_back(frame),
                    Ok(DesktopMessage::Audio {
                        samples,
                        samples_per_channel,
                    }) => {
                        self.audio
                            .queue_audio(&samples)
                            .map_err(anyhow::Error::msg)?;
                        self.submitted_audio_samples = self
                            .submitted_audio_samples
                            .saturating_add(samples_per_channel as u64);
                        self.start_audio_if_ready(false);
                    }
                    Ok(DesktopMessage::VideoEnd(video_end_pts)) => {
                        self.video_end_pts = video_end_pts;
                    }
                    Ok(DesktopMessage::VideoFinished) => self.video_finished = true,
                    Ok(DesktopMessage::ClipFinished) => return Ok(()),
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(anyhow!("decode worker disconnected"));
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }

        fn finish(mut self) -> Result<()> {
            self.start_audio_if_ready(true);
            while self.audio.size() > 0 {
                self.handle_events()?;
                self.render_due_video()?;
                thread::sleep(SCHEDULER_INTERVAL);
            }
            thread::sleep(Duration::from_secs_f64(
                self.device_buffer_samples as f64 / f64::from(self.sample_rate),
            ));
            self.render_due_video_at(self.submitted_audio_samples)
        }

        fn start_audio_if_ready(&mut self, force: bool) {
            if !self.audio_started
                && (force || self.queued_audio_samples() >= self.prebuffer_samples())
            {
                self.audio.resume();
                self.audio_started = true;
                self.audio_clock.reset(Instant::now());
            }
        }

        fn render_due_video(&mut self) -> Result<()> {
            if !self.audio_started {
                return Ok(());
            }

            let audio_pts = self.played_audio_samples();
            self.render_due_video_at(audio_pts)
        }

        fn render_due_video_at(&mut self, audio_pts: u64) -> Result<()> {
            let mut due_frame = None;
            let mut dropped_frames = 0_u64;

            while self
                .video_queue
                .front()
                .is_some_and(|frame| self.frame_is_due(frame, audio_pts))
            {
                let frame = self.video_queue.pop_front().unwrap();
                let frame_pts = frame.pts().unwrap_or_default();
                if self
                    .last_rendered_video_pts
                    .is_some_and(|last_pts| frame_pts <= last_pts)
                {
                    dropped_frames += 1;
                    continue;
                }
                if due_frame.replace(frame).is_some() {
                    dropped_frames += 1;
                }
            }

            if dropped_frames > 0 {
                log::trace!(
                    "dropped {dropped_frames} late desktop video frame(s) at audio sample {audio_pts}"
                );
            }
            if let Some(frame) = due_frame {
                self.render_video_frame(&frame)?;
                self.last_rendered_video_pts = frame.pts();
            }
            self.handle_video_starvation(audio_pts)
        }

        fn handle_video_starvation(&mut self, audio_pts: u64) -> Result<()> {
            let expected_video_pts = (audio_pts as i64)
                .rescale(Rational(1, self.sample_rate as i32), self.video_time_base)
                .max(0);
            let last_video_pts = self.last_rendered_video_pts.unwrap_or_default();
            let next_video_pts = self
                .video_queue
                .front()
                .and_then(|frame| frame.pts())
                .unwrap_or(i64::MAX);
            let starved =
                expected_video_pts > last_video_pts + 1 && next_video_pts > expected_video_pts;
            let reached_video_end = self
                .video_end_pts
                .is_some_and(|video_end_pts| expected_video_pts >= video_end_pts);
            let now = Instant::now();
            let may_report = self.last_starvation_report.is_none_or(|last_report| {
                now.duration_since(last_report) >= Duration::from_secs(1)
            });

            if starved && !self.video_finished && !reached_video_end && may_report {
                log::debug!(
                    "desktop video queue starved: expected pts {expected_video_pts}, last rendered \
                     pts {last_video_pts}, queued frames {}",
                    self.video_queue.len()
                );
                self.last_starvation_report = Some(now);
            }

            if (self.video_finished || reached_video_end)
                && starved
                && expected_video_pts - last_video_pts > VIDEO_STARVATION_GRACE_FRAMES
            {
                self.render_black_frame();
                self.last_rendered_video_pts = Some(expected_video_pts);
            }
            Ok(())
        }

        fn frame_is_due(&self, frame: &frame::Video, audio_pts: u64) -> bool {
            let frame_pts = frame.pts().unwrap_or_default().max(0);
            let frame_audio_pts =
                video_pts_in_audio_samples(frame_pts, self.video_time_base, self.sample_rate);
            frame_audio_pts <= audio_pts
        }

        fn render_video_frame(&mut self, frame: &frame::Video) -> Result<()> {
            self.texture
                .update_yuv(
                    None,
                    frame.data(0),
                    frame.stride(0),
                    frame.data(1),
                    frame.stride(1),
                    frame.data(2),
                    frame.stride(2),
                )
                .map_err(|error| anyhow!("{error}"))?;

            self.canvas.clear();
            self.canvas
                .copy(&self.texture, None, None)
                .map_err(anyhow::Error::msg)?;
            self.canvas.present();
            Ok(())
        }

        fn render_black_frame(&mut self) {
            self.canvas.set_draw_color(Color::RGB(0, 0, 0));
            self.canvas.clear();
            self.canvas.present();
        }

        fn queued_audio_samples(&self) -> u64 {
            u64::from(self.audio.size()) / (AUDIO_CHANNELS * size_of::<f32>()) as u64
        }

        fn played_audio_samples(&mut self) -> u64 {
            self.audio_clock.position(
                self.submitted_audio_samples,
                self.queued_audio_samples(),
                Instant::now(),
            )
        }

        fn prebuffer_samples(&self) -> u64 {
            u64::from(self.sample_rate) * AUDIO_PREBUFFER_MS / 1_000
        }

        fn max_queue_samples(&self) -> u64 {
            u64::from(self.sample_rate) * AUDIO_MAX_QUEUE_MS / 1_000
        }

        fn handle_events(&mut self) -> Result<()> {
            if self.event_pump.poll_iter().any(|event| {
                matches!(
                    event,
                    Event::Quit { .. }
                        | Event::KeyDown {
                            keycode: Some(Keycode::Escape),
                            ..
                        }
                )
            }) {
                return Err(PlaybackStopped.into());
            }
            Ok(())
        }
    }

    struct AudioMasterClock {
        sample_rate: u32,
        device_buffer_samples: u64,
        last_consumed_samples: u64,
        anchor_samples: u64,
        anchor_time: Instant,
    }

    impl AudioMasterClock {
        fn new(sample_rate: u32, device_buffer_samples: u64) -> Self {
            Self {
                sample_rate,
                device_buffer_samples,
                last_consumed_samples: 0,
                anchor_samples: 0,
                anchor_time: Instant::now(),
            }
        }

        fn reset(&mut self, now: Instant) {
            self.last_consumed_samples = 0;
            self.anchor_samples = 0;
            self.anchor_time = now;
        }

        fn position(&mut self, submitted: u64, queued: u64, now: Instant) -> u64 {
            let consumed = submitted.saturating_sub(queued);
            if consumed != self.last_consumed_samples {
                self.last_consumed_samples = consumed;
                self.anchor_samples = consumed.saturating_sub(self.device_buffer_samples);
                self.anchor_time = now;
            }

            let elapsed_samples = (now.duration_since(self.anchor_time).as_secs_f64()
                * f64::from(self.sample_rate)) as u64;
            self.anchor_samples
                .saturating_add(elapsed_samples)
                .min(consumed)
        }
    }

    fn video_pts_in_audio_samples(
        video_pts: i64,
        video_time_base: Rational,
        sample_rate: u32,
    ) -> u64 {
        video_pts
            .rescale(video_time_base, Rational(1, sample_rate as i32))
            .max(0) as u64
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn audio_clock_interpolates_between_device_buffer_requests() {
            let start = Instant::now();
            let mut clock = AudioMasterClock::new(48_000, 1_024);
            clock.reset(start);

            assert_eq!(clock.position(4_800, 3_776, start), 0);
            assert_eq!(
                clock.position(4_800, 3_776, start + Duration::from_millis(10)),
                480
            );
            assert_eq!(
                clock.position(4_800, 3_776, start + Duration::from_millis(30)),
                1_024
            );
        }

        #[test]
        fn audio_clock_reanchors_when_sdl_requests_another_buffer() {
            let start = Instant::now();
            let mut clock = AudioMasterClock::new(48_000, 1_024);
            clock.reset(start);

            assert_eq!(clock.position(4_800, 2_752, start), 1_024);
            assert_eq!(
                clock.position(4_800, 2_752, start + Duration::from_millis(10)),
                1_504
            );
        }

        #[test]
        fn video_pts_are_converted_to_audio_clock_samples() {
            assert_eq!(
                video_pts_in_audio_samples(25, Rational(1, 25), 48_000),
                48_000
            );
            assert_eq!(
                video_pts_in_audio_samples(1, Rational(1, 25), 48_000),
                1_920
            );
        }
    }
}
