mod cli;
mod clock;
mod config;
mod live;
mod media_info;
mod output;
mod playout;

use crate::{
    cli::{Args, resolve_inputs},
    config::OutputConfig,
    live::{LiveOverrideOutput, LiveReceiver, spawn_rtmp_listener},
    media_info::print_media_info,
    output::{FrameOutput, Output, PlaybackStopped},
    playout::{Timeline, play_clip, write_fallback},
};
use anyhow::{Context, Result, anyhow};
use clap::Parser;
use env_logger::{Builder, Env};
use ffmpeg_next::util::log::{self as ff_log, Level};
use log::*;

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

    #[cfg(feature = "desktop")]
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

    fn play(
        &mut self,
        path: &str,
        seek_seconds: Option<f64>,
        live: &mut Option<LiveReceiver>,
    ) -> Result<ClipResult> {
        #[cfg(feature = "desktop")]
        if self.output.is_desktop() {
            let config = self.config.clone();
            let fallback_duration = self.fallback_duration;
            let mut timeline = self.timeline;
            let path = path.to_string();
            let mut live_for_worker = live.take();
            let operation = self.output.run_desktop(move |output| {
                let result = if let Some(live) = live_for_worker.as_mut() {
                    let mut output = LiveOverrideOutput::new(output, live);
                    play_to_output(
                        &path,
                        &config,
                        &mut timeline,
                        &mut output,
                        fallback_duration,
                        seek_seconds,
                    )
                } else {
                    play_to_output(
                        &path,
                        &config,
                        &mut timeline,
                        output,
                        fallback_duration,
                        seek_seconds,
                    )
                };
                (result, timeline, live_for_worker)
            });

            return match operation {
                Ok((result, timeline, live_for_worker)) => {
                    self.timeline = timeline;
                    *live = live_for_worker;
                    result
                }
                Err(error) if error.downcast_ref::<PlaybackStopped>().is_some() => {
                    Ok(ClipResult::Stopped)
                }
                Err(error) => Err(error),
            };
        }

        if let Some(live) = live.as_mut() {
            let mut output = LiveOverrideOutput::new(&mut self.output, live);
            play_to_output(
                path,
                &self.config,
                &mut self.timeline,
                &mut output,
                self.fallback_duration,
                seek_seconds,
            )
        } else {
            play_to_output(
                path,
                &self.config,
                &mut self.timeline,
                &mut self.output,
                self.fallback_duration,
                seek_seconds,
            )
        }
    }

    fn finish(self) -> Result<()> {
        self.output.finish()
    }
}

fn init_ffmpeg() -> Result<()> {
    ffmpeg_next::init().context("failed to initialize FFmpeg")?;
    ff_log::set_level(Level::Warning);
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

fn init_logger() {
    let env = Env::default()
        .filter_or("MY_LOG_LEVEL", "trace")
        .write_style_or("MY_LOG_STYLE", "always");

    Builder::from_env(env)
        .format_timestamp(None)
        .format_level(true)
        .format_target(false)
        .init();
}

fn main() -> Result<()> {
    init_logger();

    let args = Args::parse();
    if args.inputs.is_empty() {
        return Err(anyhow!(
            "please provide at least one input file, directory, or glob pattern"
        ));
    }
    let inputs = resolve_inputs(&args.inputs)?;
    if !args.seek.is_finite() || args.seek < 0.0 {
        return Err(anyhow!("--seek must be a non-negative number"));
    }
    if args.hls_vtt_subtitles && args.hls_variants.is_empty() {
        return Err(anyhow!(
            "--hls-vtt-subtitles requires at least one --hls-variant so subtitles can be linked from master.m3u8"
        ));
    }

    let config = OutputConfig::default();
    let live_config = config.clone();
    let mut playout = if args.desktop() {
        #[cfg(feature = "desktop")]
        {
            Playout::open_desktop(config, args.fallback_duration)?
        }
        #[cfg(not(feature = "desktop"))]
        {
            return Err(anyhow!(
                "--desktop is not available because this binary was built without the desktop feature"
            ));
        }
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
    let mut live = args
        .rtmp_live
        .clone()
        .map(|url| spawn_rtmp_listener(url, live_config));

    for (index, path) in inputs.iter().enumerate() {
        print_media_info(path);
        let seek_seconds = (index == 0 && args.seek > 0.0).then_some(args.seek);
        match playout.play(path, seek_seconds, &mut live)? {
            ClipResult::Played => {}
            ClipResult::Fallback { reason } => {
                error!("failed while playing {path}: {reason}; fallback generated");
            }
            ClipResult::Stopped => return Ok(()),
        }
    }

    playout.finish()
}
