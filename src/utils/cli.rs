use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, anyhow};
use clap::Parser;
use glob::glob;

use crate::config::{HlsVariant, OutputSize};

#[derive(Parser, Debug)]
pub(crate) struct Args {
    /// Input files, directories, or glob patterns
    pub(crate) inputs: Vec<String>,

    /// Output file or URL, e.g. out.mp4 or rtmp://host/live/stream
    #[cfg_attr(
        feature = "desktop",
        arg(
            short,
            long,
            required_unless_present_any = ["desktop", "hls"],
            conflicts_with_all = ["desktop", "hls"]
        )
    )]
    #[cfg_attr(
        not(feature = "desktop"),
        arg(short, long, required_unless_present = "hls", conflicts_with = "hls")
    )]
    pub(crate) output: Option<String>,

    /// Play video and audio in an SDL2 desktop window
    #[cfg(feature = "desktop")]
    #[arg(long, conflicts_with_all = ["output", "hls"])]
    pub(crate) desktop: bool,

    /// Publish a live HLS playlist, e.g. /var/www/live/index.m3u8
    #[cfg_attr(
        feature = "desktop",
        arg(long, value_name = "PLAYLIST", conflicts_with_all = ["output", "desktop"])
    )]
    #[cfg_attr(
        not(feature = "desktop"),
        arg(long, value_name = "PLAYLIST", conflicts_with = "output")
    )]
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

    /// Output size as WIDTH:HEIGHT. Defaults to 1024:576.
    #[arg(long, value_name = "WIDTH:HEIGHT")]
    pub(crate) size: Option<OutputSize>,

    /// RTMP listen URL for live override, e.g. rtmp://0.0.0.0:1935/live/input
    #[arg(long, value_name = "URL")]
    pub(crate) rtmp_live: Option<String>,

    /// Duration in seconds used when an input is missing or cannot be decoded
    #[arg(long, default_value_t = 10.0)]
    pub(crate) fallback_duration: f64,
}

impl Args {
    pub(crate) fn desktop(&self) -> bool {
        #[cfg(feature = "desktop")]
        {
            self.desktop
        }
        #[cfg(not(feature = "desktop"))]
        {
            false
        }
    }
}

pub(crate) fn resolve_inputs(inputs: &[String]) -> Result<Vec<String>> {
    let mut resolved = Vec::new();
    let mut seen = HashSet::new();

    for input in inputs {
        let paths = resolve_input(input)?;
        for path in paths {
            if seen.insert(path.clone()) {
                resolved.push(path_to_string(path)?);
            }
        }
    }

    if resolved.is_empty() {
        return Err(anyhow!("input expansion produced no playable files"));
    }

    Ok(resolved)
}

fn resolve_input(input: &str) -> Result<Vec<PathBuf>> {
    if contains_glob_pattern(input) {
        let mut matches = Vec::new();
        for entry in glob(input).with_context(|| format!("invalid input glob pattern: {input}"))? {
            let path = entry.with_context(|| format!("failed to read glob match for {input}"))?;
            if path.is_dir() {
                matches.extend(resolve_directory(&path)?);
            } else if is_supported_media_file(&path) {
                matches.push(path);
            }
        }
        matches.sort();
        return Ok(matches);
    }

    let path = PathBuf::from(input);
    if path.is_dir() {
        return resolve_directory(&path);
    }

    Ok(vec![path])
}

fn resolve_directory(path: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(path)
        .with_context(|| format!("failed to read input directory {}", path.display()))?
    {
        let entry = entry.with_context(|| format!("failed to read entry in {}", path.display()))?;
        let entry_path = entry.path();
        if entry_path.is_file() && is_supported_media_file(&entry_path) {
            files.push(entry_path);
        }
    }
    files.sort();
    Ok(files)
}

fn contains_glob_pattern(input: &str) -> bool {
    input.contains('*') || input.contains('?') || input.contains('[')
}

fn is_supported_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "aac"
                    | "avi"
                    | "flac"
                    | "m4a"
                    | "m4v"
                    | "mkv"
                    | "mov"
                    | "mp3"
                    | "mp4"
                    | "mpeg"
                    | "mpg"
                    | "ogg"
                    | "opus"
                    | "ts"
                    | "wav"
                    | "webm"
            )
        })
        .unwrap_or(false)
}

fn path_to_string(path: PathBuf) -> Result<String> {
    path.into_os_string().into_string().map_err(|path| {
        anyhow!(
            "input path is not valid UTF-8: {}",
            PathBuf::from(path).display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_inputs;
    use std::{
        fs::{self, File},
        path::Path,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn expands_directories_and_globs_in_stable_order() {
        let root = test_dir("input-expand");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        touch(&first.join("b.mp4"));
        touch(&first.join("a.mp3"));
        touch(&first.join("subtitle.vtt"));
        touch(&second.join("c.mov"));
        touch(&second.join("ignore.txt"));

        let inputs = vec![
            first.to_string_lossy().into_owned(),
            second.join("*.mov").to_string_lossy().into_owned(),
        ];
        let resolved = resolve_inputs(&inputs).unwrap();

        assert_eq!(
            resolved,
            vec![
                first.join("a.mp3").to_string_lossy().into_owned(),
                first.join("b.mp4").to_string_lossy().into_owned(),
                second.join("c.mov").to_string_lossy().into_owned(),
            ]
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn keeps_plain_missing_inputs_for_fallback_handling() {
        let resolved = resolve_inputs(&["missing.mp4".to_string()]).unwrap();
        assert_eq!(resolved, vec!["missing.mp4"]);
    }

    fn touch(path: &Path) {
        File::create(path).unwrap();
    }

    fn test_dir(prefix: &str) -> std::path::PathBuf {
        let id = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{id}"))
    }
}
