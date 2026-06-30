use ffmpeg_next::Rational;

#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub sample_rate: u32,
    pub video_time_base: Rational,
    pub audio_time_base: Rational,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            width: 1024,
            height: 576,
            fps: 25,
            sample_rate: 48_000,
            video_time_base: Rational(1, 25),
            audio_time_base: Rational(1, 48_000),
        }
    }
}
