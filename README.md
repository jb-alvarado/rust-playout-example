# Rust Playout Example

A small Rust example for continuous audio and video playout using the FFmpeg
libraries.

It:

- reads a list of files
- decodes, scales, and resamples audio and video
- generates continuous timestamps
- outputs in real time to a media file or RTMP stream
- generates black video and silence for missing media streams
- uses a configurable fallback when an input is missing or cannot be decoded

This is an architectural example, not a production-ready 24/7 playout system.

## Requirements

Debian/Ubuntu:

```bash
sudo apt install pkg-config clang libavformat-dev libavcodec-dev \
  libavutil-dev libswscale-dev libswresample-dev libsdl2-dev
```

Fedora:

```bash
sudo dnf install ffmpeg-devel SDL2-devel clang pkgconf-pkg-config
```

## Build

```bash
cargo build
```

## Usage

Write a media file:

```bash
cargo run -- \
  --output output.mp4 \
  input1.mp4 input2.mp4 input3.mp3
```

Publish an RTMP stream:

```bash
cargo run -- \
  --output rtmp://127.0.0.1/live/stream \
  input1.mp4 input2.mp4
```

RTMP output automatically uses the FLV container.

Publish a live HLS playlist:

```bash
cargo run -- \
  --hls public/live/index.m3u8 \
  input1.mp4 input2.mp4
```

HLS uses two-second segments and keeps the latest five entries in the playlist.

Publish adaptive HLS with a master playlist and multiple renditions:

```bash
cargo run -- \
  --hls public/live/index.m3u8 \
  --hls-variant 360p:640x360:800k:96k \
  --hls-variant 720p:1280x720:2800k:128k \
  --hls-vtt-subtitles \
  input1.mp4 input2.mp4
```

With `--hls-variant`, FFmpeg writes `public/live/master.m3u8` plus one
variant playlist per rendition in the same directory, for example
`public/live/360p_index.m3u8` and `public/live/720p_index.m3u8`. Segments are
also written to the same directory, for example `public/live/360p_segment_000.ts`.
The format is
`NAME:WIDTHxHEIGHT:VIDEO_BITRATE[:AUDIO_BITRATE]`; audio bitrate defaults to
`128k`. Variant names must be unique and may contain only ASCII letters,
numbers, `_`, and `-`.

With `--hls-vtt-subtitles`, sidecar WebVTT subtitles are included when a `.vtt`
file with the same base name as an input exists. For example, `input1.mp4` uses
`input1.vtt`. Missing sidecar files are ignored. This option requires at least
one `--hls-variant`, because the subtitles are linked from `master.m3u8`.

Play through an SDL2 window:

```bash
cargo run -- --desktop input1.mp4 input2.mp4
```

The fallback duration defaults to 10 seconds and can be changed:

```bash
cargo run -- \
  --output output.mp4 \
  --fallback-duration 5 \
  input1.mp4 missing.mp4 input3.mp4
```

Start playback at an offset in the first input only:

```bash
cargo run -- \
  --output output.mp4 \
  --seek 30 \
  input1.mp4 input2.mp4
```

`--seek` is specified in seconds and is applied only to the first input file.

Start an RTMP live override listener:

```bash
cargo run -- \
  --hls public/live/index.m3u8 \
  --rtmp-live rtmp://0.0.0.0:1935/live/input \
  input1.mp4 input2.mp4
```

Publish a temporary live source into it:

```bash
ffmpeg -re -i live-source.mp4 -c copy -f flv rtmp://127.0.0.1:1935/live/input
```

When a publisher connects, file playback output switches to the RTMP live input.
When the live input ends or stops delivering frames for a few seconds, file
playback output resumes. `--rtmp-live` is currently supported for encoded
outputs, not `--desktop`.
