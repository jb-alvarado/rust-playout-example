use anyhow::{Context, Result, anyhow};
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(super) struct VttCue {
    pub(super) start_ms: i64,
    pub(super) end_ms: i64,
    pub(super) text: String,
}

pub(super) fn sidecar_path(media_path: &str) -> PathBuf {
    Path::new(media_path).with_extension("vtt")
}

pub(super) fn parse_file(path: &Path) -> Result<Vec<VttCue>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read VTT sidecar {}", path.display()))?;
    parse(&content).with_context(|| format!("failed to parse VTT sidecar {}", path.display()))
}

fn parse(content: &str) -> Result<Vec<VttCue>> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();
    let mut block = Vec::new();

    for line in normalized.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            parse_block(&block, &mut cues)?;
            block.clear();
        } else {
            block.push(line);
        }
    }

    Ok(cues)
}

fn parse_block(lines: &[&str], cues: &mut Vec<VttCue>) -> Result<()> {
    if lines.is_empty() {
        return Ok(());
    }

    let first = lines[0].trim_start_matches('\u{feff}').trim();
    if first.starts_with("WEBVTT")
        || first.starts_with("NOTE")
        || first == "STYLE"
        || first == "REGION"
    {
        return Ok(());
    }

    let timing_index = lines
        .iter()
        .position(|line| line.contains("-->"))
        .ok_or_else(|| anyhow!("VTT cue is missing a timing line"))?;
    let (start_ms, end_ms) = parse_timing(lines[timing_index])?;
    if end_ms <= start_ms {
        return Err(anyhow!("VTT cue end must be after start"));
    }

    let text = lines[timing_index + 1..]
        .iter()
        .map(|line| line.trim_end())
        .collect::<Vec<_>>()
        .join("\n");
    if text.trim().is_empty() {
        return Ok(());
    }

    cues.push(VttCue {
        start_ms,
        end_ms,
        text,
    });
    Ok(())
}

fn parse_timing(line: &str) -> Result<(i64, i64)> {
    let (start, rest) = line
        .split_once("-->")
        .ok_or_else(|| anyhow!("invalid VTT timing line"))?;
    let end = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("missing VTT cue end timestamp"))?;
    Ok((parse_timestamp(start.trim())?, parse_timestamp(end)?))
}

fn parse_timestamp(value: &str) -> Result<i64> {
    let value = value.replace(',', ".");
    let parts = value.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0_i64, minutes.parse::<i64>()?, *seconds),
        [hours, minutes, seconds] => (hours.parse::<i64>()?, minutes.parse::<i64>()?, *seconds),
        _ => return Err(anyhow!("invalid VTT timestamp {value:?}")),
    };
    let (seconds, millis) = seconds
        .split_once('.')
        .ok_or_else(|| anyhow!("VTT timestamp must include milliseconds"))?;
    let seconds = seconds.parse::<i64>()?;
    let millis = parse_milliseconds(millis)?;

    Ok(((hours * 60 + minutes) * 60 + seconds) * 1_000 + millis)
}

fn parse_milliseconds(value: &str) -> Result<i64> {
    let mut millis = value.chars().take(3).collect::<String>();
    if millis.is_empty() || !millis.chars().all(|ch| ch.is_ascii_digit()) {
        return Err(anyhow!("invalid VTT millisecond value"));
    }
    while millis.len() < 3 {
        millis.push('0');
    }
    Ok(millis.parse::<i64>()?)
}
