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

## FFmpeg bindings

This project uses [`ffmpeg-next`](https://crates.io/crates/ffmpeg-next). A
handful of low-level operations (RTMP listen-mode input, WebVTT subtitle
stream setup) fall back to raw FFmpeg FFI because `ffmpeg-next` doesn't yet
expose a safe API for them.

[`ffmpeg-the-third`](https://github.com/shssoichiro/ffmpeg-the-third) is an
actively maintained fork worth revisiting if `ffmpeg-next` development stalls
again or if its Dictionary/format API improvements (which would remove some
of our unsafe workarounds) become worth the migration effort. No action is
needed today; both crates currently receive updates.

## Docker build

Build the application in a Debian Trixie container and export the binary to
`target/docker/rust-playout-example`:

```bash
docker compose build
docker compose up
```

The Compose service only builds and copies the release binary. It does not run
the playout application. The Docker build uses `--no-default-features`, so the
exported binary does not include SDL2 desktop playback.

```bash
./target/docker/rust-playout-example --help
```

## Requirements

Debian/Ubuntu:

```bash
sudo apt install pkg-config clang libavformat-dev libavcodec-dev \
  libavdevice-dev libavfilter-dev libavutil-dev libswscale-dev libswresample-dev libsdl2-dev libclang-dev
```

Fedora:

```bash
sudo dnf install ffmpeg-devel SDL2-devel clang pkgconf-pkg-config
```

## Build

```bash
cargo build
```

Desktop playback is enabled by default. To build the same feature set as the
Docker build, disable default features:

```bash
cargo build --release --no-default-features
```

## Usage

Write a media file:

```bash
cargo run -- \
  --output output.mp4 \
  input1.mp4 input2.mp4 input3.mp3
```

Inputs can also be directories or glob patterns. Directory inputs are expanded
to supported media files in sorted order; glob matches are sorted as well:

```bash
cargo run -- \
  --output output.mp4 \
  media/day1 media/day2 "media/**/*.mp4"
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
playback output resumes.
