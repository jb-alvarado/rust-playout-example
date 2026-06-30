use clap::Parser;

#[derive(Parser, Debug)]
pub(crate) struct Args {
    /// Input playlist files
    pub(crate) inputs: Vec<String>,

    /// Output file or URL, e.g. out.mp4 or rtmp://host/live/stream
    #[arg(short, long, required_unless_present = "desktop")]
    pub(crate) output: Option<String>,

    /// Play video and audio in an SDL2 desktop window
    #[arg(long, conflicts_with = "output")]
    pub(crate) desktop: bool,

    /// Duration in seconds used when an input is missing or cannot be decoded
    #[arg(long, default_value_t = 10.0)]
    pub(crate) fallback_duration: f64,
}
