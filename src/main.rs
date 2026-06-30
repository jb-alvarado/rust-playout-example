mod cli;
mod clock;
mod config;
mod media_info;
mod output;
mod playout;

use crate::{
    cli::Args,
    config::OutputConfig,
    media_info::print_media_info,
    output::{FrameOutput, Output, PlaybackStopped},
    playout::{Timeline, play_clip, write_fallback},
};
use anyhow::{Context, Result, anyhow};
use clap::Parser;
use ffmpeg_next::util::log::{self, Level};

#[derive(Debug, Clone, PartialEq, Eq)]
enum ClipResult {
    Played,
    Fallback { reason: String },
    Stopped,
}

struct Playout {
    config: OutputConfig,
    output: Output,
    timeline: Timeline,
    fallback_duration: f64,
}

impl Playout {
    fn open(output_url: &str, config: OutputConfig, fallback_duration: f64) -> Result<Self> {
        Self::validate_fallback_duration(fallback_duration)?;
        init_ffmpeg()?;
        let output = Output::open(output_url, &config)?;

        Ok(Self::with_output(config, output, fallback_duration))
    }

    fn open_desktop(config: OutputConfig, fallback_duration: f64) -> Result<Self> {
        Self::validate_fallback_duration(fallback_duration)?;
        init_ffmpeg()?;
        let output = Output::open_desktop(&config)?;

        Ok(Self::with_output(config, output, fallback_duration))
    }

    fn open_hls(
        playlist: &str,
        config: OutputConfig,
        fallback_duration: f64,
        hls_variants: &[config::HlsVariant],
        hls_vtt_subtitles: bool,
    ) -> Result<Self> {
        Self::validate_fallback_duration(fallback_duration)?;
        init_ffmpeg()?;
        let output = Output::open_hls(playlist, &config, hls_variants, hls_vtt_subtitles)?;

        Ok(Self::with_output(config, output, fallback_duration))
    }

    fn validate_fallback_duration(fallback_duration: f64) -> Result<()> {
        if !fallback_duration.is_finite() || fallback_duration <= 0.0 {
            return Err(anyhow!("fallback duration must be a positive number"));
        }
        Ok(())
    }

    fn with_output(config: OutputConfig, output: Output, fallback_duration: f64) -> Self {
        Self {
            config,
            output,
            timeline: Timeline::new(),
            fallback_duration,
        }
    }

    fn play(&mut self, path: &str, seek_seconds: Option<f64>) -> Result<ClipResult> {
        if self.output.is_desktop() {
            let config = self.config.clone();
            let fallback_duration = self.fallback_duration;
            let mut timeline = self.timeline;
            let path = path.to_string();
            let operation = self.output.run_desktop(move |output| {
                let result = play_to_output(
                    &path,
                    &config,
                    &mut timeline,
                    output,
                    fallback_duration,
                    seek_seconds,
                );
                (result, timeline)
            });

            return match operation {
                Ok((result, timeline)) => {
                    self.timeline = timeline;
                    result
                }
                Err(error) if error.downcast_ref::<PlaybackStopped>().is_some() => {
                    Ok(ClipResult::Stopped)
                }
                Err(error) => Err(error),
            };
        }

        play_to_output(
            path,
            &self.config,
            &mut self.timeline,
            &mut self.output,
            self.fallback_duration,
            seek_seconds,
        )
    }

    fn finish(self) -> Result<()> {
        self.output.finish()
    }
}

fn init_ffmpeg() -> Result<()> {
    ffmpeg_next::init().context("failed to initialize FFmpeg")?;
    log::set_level(Level::Warning);
    Ok(())
}

fn play_to_output<O: FrameOutput>(
    path: &str,
    config: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    fallback_duration: f64,
    seek_seconds: Option<f64>,
) -> Result<ClipResult> {
    match play_clip(path, config, timeline, output, seek_seconds) {
        Ok(()) => Ok(ClipResult::Played),
        Err(error) if error.downcast_ref::<PlaybackStopped>().is_some() => Ok(ClipResult::Stopped),
        Err(error) => {
            let reason = format!("{error:#}");
            write_fallback(config, timeline, output, fallback_duration)
                .with_context(|| format!("failed to generate fallback for {path}"))?;
            Ok(ClipResult::Fallback { reason })
        }
    }
}

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();
    if args.inputs.is_empty() {
        return Err(anyhow!("please provide at least one input file"));
    }
    if !args.seek.is_finite() || args.seek < 0.0 {
        return Err(anyhow!("--seek must be a non-negative number"));
    }
    if args.hls_vtt_subtitles && args.hls_variants.is_empty() {
        return Err(anyhow!(
            "--hls-vtt-subtitles requires at least one --hls-variant so subtitles can be linked from master.m3u8"
        ));
    }

    let config = OutputConfig::default();
    let mut playout = if args.desktop {
        Playout::open_desktop(config, args.fallback_duration)?
    } else if let Some(playlist) = args.hls.as_deref() {
        Playout::open_hls(
            playlist,
            config,
            args.fallback_duration,
            &args.hls_variants,
            args.hls_vtt_subtitles,
        )?
    } else {
        Playout::open(
            args.output
                .as_deref()
                .ok_or_else(|| anyhow!("missing output"))?,
            config,
            args.fallback_duration,
        )?
    };

    for (index, path) in args.inputs.iter().enumerate() {
        print_media_info(path);
        let seek_seconds = (index == 0 && args.seek > 0.0).then_some(args.seek);
        match playout.play(path, seek_seconds)? {
            ClipResult::Played => {}
            ClipResult::Fallback { reason } => {
                eprintln!("failed while playing {path}: {reason}; fallback generated");
            }
            ClipResult::Stopped => return Ok(()),
        }
    }

    playout.finish()
}
