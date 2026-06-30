use crate::config::HlsVariant;
use anyhow::{Context, Result, anyhow};
use ffmpeg_next as ffmpeg;
use std::{collections::HashSet, fs, io::ErrorKind, path::Path, ptr};

pub(super) fn playlist_path(path: &str, variants: &[HlsVariant]) -> Result<String> {
    if variants.is_empty() {
        return Ok(path.to_string());
    }

    let path = Path::new(path);
    let file_name = path
        .file_name()
        .and_then(|file_name| file_name.to_str())
        .context("HLS playlist path must include a file name")?;
    Ok(path
        .with_file_name(format!("%v_{file_name}"))
        .to_string_lossy()
        .into_owned())
}

pub(super) fn validate_variants(variants: &[HlsVariant]) -> Result<()> {
    let mut names = HashSet::new();
    for variant in variants {
        if !names.insert(variant.name.as_str()) {
            return Err(anyhow!("duplicate HLS variant name {}", variant.name));
        }
    }
    Ok(())
}

pub(super) fn close_preopened_output(
    octx: &mut ffmpeg::format::context::Output,
    path: &str,
) -> Result<()> {
    unsafe {
        let context = octx.as_mut_ptr();
        if !(*context).pb.is_null() {
            let result = ffmpeg::ffi::avio_close((*context).pb);
            (*context).pb = ptr::null_mut();
            if result < 0 {
                return Err(ffmpeg::Error::from(result).into());
            }
        }
    }

    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => {
            Err(error).with_context(|| format!("failed to remove HLS placeholder {path}"))
        }
    }
}

pub(super) fn segment_pattern(path: &str) -> String {
    Path::new(path)
        .with_file_name("%v_segment_%03d.ts")
        .to_string_lossy()
        .into_owned()
}

pub(super) fn var_stream_map(variants: &[HlsVariant], include_subtitles: bool) -> String {
    variants
        .iter()
        .enumerate()
        .map(|(index, variant)| {
            if include_subtitles && index == 0 {
                format!("v:{index},a:{index},s:0,sgroup:subs,name:{}", variant.name)
            } else {
                format!("v:{index},a:{index},name:{}", variant.name)
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}
