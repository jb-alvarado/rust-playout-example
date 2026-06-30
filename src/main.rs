mod cli;
mod clock;
mod config;
mod output;
mod playout;

use crate::{
    cli::Args,
    config::OutputConfig,
    output::{FrameOutput, Output, PlaybackStopped},
    playout::{Timeline, play_clip, write_fallback},
};
use anyhow::{Context, Result, anyhow};
use clap::Parser;

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
        ffmpeg_next::init().context("failed to initialize FFmpeg")?;
        let output = Output::open(output_url, &config)?;

        Ok(Self::with_output(config, output, fallback_duration))
    }

    fn open_desktop(config: OutputConfig, fallback_duration: f64) -> Result<Self> {
        Self::validate_fallback_duration(fallback_duration)?;
        ffmpeg_next::init().context("failed to initialize FFmpeg")?;
        let output = Output::open_desktop(&config)?;

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

    fn play(&mut self, path: &str) -> Result<ClipResult> {
        if self.output.is_desktop() {
            let config = self.config.clone();
            let fallback_duration = self.fallback_duration;
            let mut timeline = self.timeline;
            let path = path.to_string();
            let operation = self.output.run_desktop(move |output| {
                let result =
                    play_to_output(&path, &config, &mut timeline, output, fallback_duration);
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
        )
    }

    fn finish(self) -> Result<()> {
        self.output.finish()
    }
}

fn play_to_output<O: FrameOutput>(
    path: &str,
    config: &OutputConfig,
    timeline: &mut Timeline,
    output: &mut O,
    fallback_duration: f64,
) -> Result<ClipResult> {
    match play_clip(path, config, timeline, output) {
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

    let config = OutputConfig::default();
    let mut playout = if args.desktop {
        Playout::open_desktop(config, args.fallback_duration)?
    } else {
        Playout::open(
            args.output
                .as_deref()
                .ok_or_else(|| anyhow!("missing output"))?,
            config,
            args.fallback_duration,
        )?
    };

    for path in &args.inputs {
        eprintln!("playing: {path}");
        match playout.play(path)? {
            ClipResult::Played => {}
            ClipResult::Fallback { reason } => {
                eprintln!("failed while playing {path}: {reason}; fallback generated");
            }
            ClipResult::Stopped => return Ok(()),
        }
    }

    playout.finish()
}
