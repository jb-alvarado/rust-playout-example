use crate::{config::OutputConfig, output::FrameOutput};
use anyhow::{Result, anyhow};
use ffmpeg_next::{
    Rational, Rescale, codec, format, frame, media,
    software::{resampling, scaling},
    util::{channel_layout::ChannelLayout, format::pixel::Pixel, format::sample::Sample},
};

#[derive(Clone, Copy)]
pub(crate) struct Timeline {
    video_pts: i64,
    audio_pts: i64,
}

impl Timeline {
    pub(crate) fn new() -> Self {
        Self {
            video_pts: 0,
            audio_pts: 0,
        }
    }
}

/// Plays one file into the continuous output timeline.
///
/// Input PTS are replaced with continuous timeline PTS. If only one media type
/// exists, the missing counterpart is synthesized.
pub(crate) fn play_clip<O: FrameOutput>(
    path: &str,
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
) -> Result<()> {
    let mut ictx = format::input(path)?;

    let video_stream = ictx.streams().best(media::Type::Video);
    let audio_stream = ictx.streams().best(media::Type::Audio);
    if video_stream.is_none() && audio_stream.is_none() {
        return Err(anyhow!("input contains no audio or video stream"));
    }

    let mut video = match video_stream {
        Some(ref stream) => Some(VideoDecoder::new(stream, cfg)?),
        None => None,
    };
    let mut audio = match audio_stream {
        Some(ref stream) => Some(AudioDecoder::new(stream, cfg)?),
        None => None,
    };

    let video_index = video_stream.as_ref().map(format::stream::Stream::index);
    let audio_index = audio_stream.as_ref().map(format::stream::Stream::index);
    let video_duration_us = video_stream.as_ref().and_then(stream_duration_us);
    let mut video_finished = video.is_none();
    let mut decoded_video_frames = 0_i64;
    let mut decoded_audio_samples = 0_i64;

    let video_end_pts = video_duration_us.map(|duration_us| {
        timeline.video_pts
            + div_ceil(i128::from(duration_us) * i128::from(cfg.fps), 1_000_000) as i64
    });
    output.set_video_end(video_end_pts)?;

    if video_finished {
        output.video_finished()?;
    }

    for (stream, packet) in ictx.packets() {
        if Some(stream.index()) == video_index {
            if !video_finished && let Some(video) = video.as_mut() {
                video.decoder.send_packet(&packet)?;
                receive_video_frames(video, timeline, output, &mut decoded_video_frames)?;
            }
        } else if Some(stream.index()) == audio_index
            && let Some(audio) = audio.as_mut()
        {
            audio.decoder.send_packet(&packet)?;
            receive_audio_frames(audio, timeline, output, &mut decoded_audio_samples)?;
        }
    }

    finish_video(
        &mut video,
        timeline,
        output,
        &mut decoded_video_frames,
        &mut video_finished,
    )?;
    if let Some(audio) = audio.as_mut() {
        audio.decoder.send_eof()?;
        receive_audio_frames(audio, timeline, output, &mut decoded_audio_samples)?;
    }

    if decoded_video_frames == 0 && decoded_audio_samples == 0 {
        return Err(anyhow!("input produced no decodable audio or video frames"));
    }

    synchronize_timeline(cfg, timeline, output)
}

fn finish_video<O: FrameOutput>(
    video: &mut Option<VideoDecoder>,
    timeline: &mut Timeline,
    output: &mut O,
    decoded_frames: &mut i64,
    finished: &mut bool,
) -> Result<()> {
    if *finished {
        return Ok(());
    }

    if let Some(video) = video.as_mut() {
        video.decoder.send_eof()?;
        receive_video_frames(video, timeline, output, decoded_frames)?;
    }
    output.video_finished()?;
    *finished = true;
    Ok(())
}

fn stream_duration_us(stream: &format::stream::Stream) -> Option<i64> {
    if stream.duration() > 0 {
        return Some(
            stream
                .duration()
                .rescale(stream.time_base(), Rational(1, 1_000_000)),
        );
    }

    let metadata = stream.metadata();
    metadata
        .get("DURATION")
        .or_else(|| metadata.get("duration"))
        .and_then(parse_duration_us)
}

fn parse_duration_us(duration: &str) -> Option<i64> {
    let mut parts = duration.split(':');
    let hours = parts.next()?.parse::<f64>().ok()?;
    let minutes = parts.next()?.parse::<f64>().ok()?;
    let seconds = parts.next()?.parse::<f64>().ok()?;
    if parts.next().is_some() {
        return None;
    }

    Some(((hours * 3_600.0 + minutes * 60.0 + seconds) * 1_000_000.0).round() as i64)
}

fn receive_video_frames<O: FrameOutput>(
    video: &mut VideoDecoder,
    timeline: &mut Timeline,
    output: &mut O,
    decoded_frames: &mut i64,
) -> Result<()> {
    let mut raw = frame::Video::empty();
    while video.decoder.receive_frame(&mut raw).is_ok() {
        let output_frames = video
            .frame_rate_converter
            .output_frames(raw.timestamp().or_else(|| raw.pts()));
        if output_frames == 0 {
            continue;
        }

        let mut scaled = frame::Video::empty();
        video.scaler.run(&raw, &mut scaled)?;
        for _ in 0..output_frames {
            scaled.set_pts(Some(timeline.video_pts));
            output.encode_video(&scaled)?;
            timeline.video_pts += 1;
            *decoded_frames += 1;
        }
    }
    Ok(())
}

fn receive_audio_frames<O: FrameOutput>(
    audio: &mut AudioDecoder,
    timeline: &mut Timeline,
    output: &mut O,
    decoded_samples: &mut i64,
) -> Result<()> {
    let mut raw = frame::Audio::empty();
    while audio.decoder.receive_frame(&mut raw).is_ok() {
        let mut converted = frame::Audio::empty();
        audio.resampler.run(&raw, &mut converted)?;
        let samples = converted.samples() as i64;
        converted.set_pts(Some(timeline.audio_pts));
        output.encode_audio(&converted)?;
        timeline.audio_pts += samples;
        *decoded_samples += samples;
    }
    Ok(())
}

struct VideoDecoder {
    decoder: codec::decoder::Video,
    scaler: scaling::Context,
    frame_rate_converter: FrameRateConverter,
}

impl VideoDecoder {
    fn new(stream: &format::stream::Stream, cfg: &OutputConfig) -> Result<Self> {
        let mut ctx = codec::context::Context::from_parameters(stream.parameters())?;
        ctx.set_threading(codec::threading::Config::kind(
            codec::threading::Type::Slice,
        ));
        let decoder = ctx.decoder().video()?;
        let scaler = scaling::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            Pixel::YUV420P,
            cfg.width,
            cfg.height,
            scaling::flag::Flags::BILINEAR,
        )?;
        Ok(Self {
            decoder,
            scaler,
            frame_rate_converter: FrameRateConverter::new(stream.time_base(), cfg.fps),
        })
    }
}

struct FrameRateConverter {
    input_time_base: Rational,
    output_time_base: Rational,
    first_timestamp: Option<i64>,
    next_output_frame: i64,
}

impl FrameRateConverter {
    fn new(input_time_base: Rational, output_fps: u32) -> Self {
        Self {
            input_time_base,
            output_time_base: Rational(1, output_fps as i32),
            first_timestamp: None,
            next_output_frame: 0,
        }
    }

    fn output_frames(&mut self, timestamp: Option<i64>) -> i64 {
        let Some(timestamp) = timestamp else {
            self.next_output_frame += 1;
            return 1;
        };

        let first_timestamp = *self.first_timestamp.get_or_insert(timestamp);
        let relative_timestamp = timestamp.saturating_sub(first_timestamp).max(0);
        let target_frame = relative_timestamp
            .rescale(self.input_time_base, self.output_time_base)
            .max(0);
        let output_frames = (target_frame + 1 - self.next_output_frame).max(0);
        self.next_output_frame += output_frames;
        output_frames
    }
}

struct AudioDecoder {
    decoder: codec::decoder::Audio,
    resampler: resampling::Context,
}

impl AudioDecoder {
    fn new(stream: &format::stream::Stream, cfg: &OutputConfig) -> Result<Self> {
        let ctx = codec::context::Context::from_parameters(stream.parameters())?;
        let decoder = ctx.decoder().audio()?;
        let resampler = resampling::Context::get(
            decoder.format(),
            decoder.channel_layout(),
            decoder.rate(),
            Sample::F32(format::sample::Type::Planar),
            ChannelLayout::STEREO,
            cfg.sample_rate,
        )?;
        Ok(Self { decoder, resampler })
    }
}

pub(crate) fn write_fallback<O: FrameOutput>(
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    duration: f64,
) -> Result<()> {
    let video_end = timeline.video_pts + (duration * f64::from(cfg.fps)).ceil() as i64;
    let audio_end = timeline.audio_pts + (duration * f64::from(cfg.sample_rate)).ceil() as i64;

    while timeline.video_pts < video_end || timeline.audio_pts < audio_end {
        let video_time = timeline.video_pts as f64 / f64::from(cfg.fps);
        let audio_time = timeline.audio_pts as f64 / f64::from(cfg.sample_rate);

        if timeline.video_pts < video_end
            && (timeline.audio_pts >= audio_end || video_time <= audio_time)
        {
            write_black_frames(cfg, timeline, output, 1)?;
        } else {
            let remaining = (audio_end - timeline.audio_pts) as usize;
            let samples = remaining.min(output.audio_frame_size().max(1));
            write_silence_frame(cfg, timeline, output, samples)?;
        }
    }

    synchronize_timeline(cfg, timeline, output)
}

fn synchronize_timeline<O: FrameOutput>(
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
) -> Result<()> {
    let (video_frames, audio_samples) = padding_to_sync(
        timeline.video_pts,
        timeline.audio_pts,
        cfg.fps,
        cfg.sample_rate,
    )?;

    if video_frames > 0 {
        println!(
            "padding video with {video_frames} black frame(s) ({:.6} s) to synchronize the timeline",
            video_frames as f64 / f64::from(cfg.fps)
        );
    }
    write_black_frames(cfg, timeline, output, video_frames)?;

    if audio_samples > 0 {
        println!(
            "padding audio with {audio_samples} silent sample(s) ({:.6} s) to synchronize the timeline",
            audio_samples as f64 / f64::from(cfg.sample_rate)
        );
    }
    write_silence(cfg, timeline, output, audio_samples)
}

fn padding_to_sync(
    video_pts: i64,
    audio_pts: i64,
    fps: u32,
    sample_rate: u32,
) -> Result<(i64, i64)> {
    if fps == 0 || sample_rate == 0 {
        return Err(anyhow!("fps and sample rate must be greater than zero"));
    }

    let fps = i128::from(fps);
    let sample_rate = i128::from(sample_rate);
    let mut video_end = i128::from(video_pts);
    let audio_end = i128::from(audio_pts);
    let mut video_padding = 0_i128;
    let mut audio_padding = 0_i128;

    if video_end * sample_rate < audio_end * fps {
        let target = div_ceil(audio_end * fps, sample_rate);
        video_padding = target - video_end;
        video_end = target;
    }

    if audio_end * fps < video_end * sample_rate {
        let target = div_ceil(video_end * sample_rate, fps);
        audio_padding = target - audio_end;
    }

    let video_padding =
        i64::try_from(video_padding).map_err(|_| anyhow!("video padding exceeds i64"))?;
    let audio_padding =
        i64::try_from(audio_padding).map_err(|_| anyhow!("audio padding exceeds i64"))?;

    Ok((video_padding, audio_padding))
}

fn div_ceil(numerator: i128, denominator: i128) -> i128 {
    (numerator + denominator - 1) / denominator
}

fn write_black_frames<O: FrameOutput>(
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    frames: i64,
) -> Result<()> {
    for _ in 0..frames {
        let mut black = black_video_frame(cfg);
        black.set_pts(Some(timeline.video_pts));
        output.encode_video(&black)?;
        timeline.video_pts += 1;
    }
    Ok(())
}

fn black_video_frame(cfg: &OutputConfig) -> frame::Video {
    let mut frame = frame::Video::new(Pixel::YUV420P, cfg.width, cfg.height);
    fill_plane(&mut frame, 0, 16);
    fill_plane(&mut frame, 1, 128);
    fill_plane(&mut frame, 2, 128);
    frame
}

fn fill_plane(frame: &mut frame::Video, plane: usize, value: u8) {
    let height = if plane == 0 {
        frame.height()
    } else {
        frame.height() / 2
    } as usize;
    let width = if plane == 0 {
        frame.width()
    } else {
        frame.width() / 2
    } as usize;
    let stride = frame.stride(plane);
    let data = frame.data_mut(plane);
    for y in 0..height {
        let start = y * stride;
        data[start..start + width].fill(value);
    }
}

fn write_silence_frame<O: FrameOutput>(
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    samples: usize,
) -> Result<()> {
    let mut frame = frame::Audio::new(
        Sample::F32(format::sample::Type::Planar),
        samples,
        ChannelLayout::STEREO,
    );
    frame.set_rate(cfg.sample_rate);
    frame.set_pts(Some(timeline.audio_pts));
    for plane in 0..frame.planes() {
        frame.plane_mut::<f32>(plane).fill(0.0);
    }
    output.encode_audio(&frame)?;
    timeline.audio_pts += samples as i64;
    Ok(())
}

fn write_silence<O: FrameOutput>(
    cfg: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    mut samples: i64,
) -> Result<()> {
    let frame_samples = output.audio_frame_size().max(1);
    while samples > 0 {
        let current_samples = samples.min(frame_samples as i64) as usize;
        write_silence_frame(cfg, timeline, output, current_samples)?;
        samples -= current_samples as i64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{FrameRateConverter, Rational, padding_to_sync, parse_duration_us};

    #[test]
    fn pads_short_audio_to_video_duration() {
        assert_eq!(padding_to_sync(50, 95_000, 25, 48_000).unwrap(), (0, 1_000));
    }

    #[test]
    fn pads_short_video_to_audio_duration() {
        assert_eq!(padding_to_sync(49, 96_000, 25, 48_000).unwrap(), (1, 0));
    }

    #[test]
    fn rounds_both_streams_to_a_shared_boundary() {
        assert_eq!(padding_to_sync(30, 44_101, 30, 44_100).unwrap(), (1, 1_469));
    }

    #[test]
    fn rejects_invalid_output_rates() {
        assert!(padding_to_sync(1, 1, 0, 48_000).is_err());
        assert!(padding_to_sync(1, 1, 25, 0).is_err());
    }

    #[test]
    fn converts_24_fps_to_25_fps() {
        let mut converter = FrameRateConverter::new(Rational(1, 24), 25);
        let output_frames = (0..240)
            .map(|timestamp| converter.output_frames(Some(timestamp)))
            .sum::<i64>();

        assert_eq!(output_frames, 250);
    }

    #[test]
    fn converts_30_fps_to_25_fps() {
        let mut converter = FrameRateConverter::new(Rational(1, 30), 25);
        let output_counts = (0..300)
            .map(|timestamp| converter.output_frames(Some(timestamp)))
            .collect::<Vec<_>>();

        assert_eq!(output_counts.iter().sum::<i64>(), 250);
        assert_eq!(
            output_counts.iter().filter(|frames| **frames == 0).count(),
            50
        );
    }

    #[test]
    fn parses_stream_duration_metadata() {
        assert_eq!(parse_duration_us("00:00:12.000000000"), Some(12_000_000));
        assert_eq!(parse_duration_us("01:02:03.500"), Some(3_723_500_000));
        assert_eq!(parse_duration_us("invalid"), None);
    }
}
