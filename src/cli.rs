use crate::config::HlsVariant;
use clap::Parser;

#[derive(Parser, Debug)]
pub(crate) struct Args {
    /// Input playlist files
    pub(crate) inputs: Vec<String>,

    /// Output file or URL, e.g. out.mp4 or rtmp://host/live/stream
    #[arg(
        short,
        long,
        required_unless_present_any = ["desktop", "hls"],
        conflicts_with_all = ["desktop", "hls"]
    )]
    pub(crate) output: Option<String>,

    /// Play video and audio in an SDL2 desktop window
    #[arg(long, conflicts_with_all = ["output", "hls"])]
    pub(crate) desktop: bool,

    /// Publish a live HLS playlist, e.g. /var/www/live/index.m3u8
    #[arg(long, value_name = "PLAYLIST", conflicts_with_all = ["output", "desktop"])]
    pub(crate) hls: Option<String>,

    /// Add an adaptive HLS rendition: NAME:WIDTHxHEIGHT:VIDEO_BITRATE[:AUDIO_BITRATE]
    #[arg(
        long = "hls-variant",
        value_name = "NAME:WIDTHxHEIGHT:VIDEO_BITRATE[:AUDIO_BITRATE]",
        requires = "hls"
    )]
    pub(crate) hls_variants: Vec<HlsVariant>,

    /// Include sidecar WebVTT subtitles for HLS. For input video.mp4, video.vtt is used.
    #[arg(long, requires = "hls")]
    pub(crate) hls_vtt_subtitles: bool,

    /// Seek position in seconds for the first input file only
    #[arg(long, value_name = "SECONDS", default_value_t = 0.0)]
    pub(crate) seek: f64,

    /// Duration in seconds used when an input is missing or cannot be decoded
    #[arg(long, default_value_t = 10.0)]
    pub(crate) fallback_duration: f64,
}
