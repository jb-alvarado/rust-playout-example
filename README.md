# Rust Playout Example

A small Rust example for continuous audio and video playout using the FFmpeg
libraries.

It:

- reads a playlist of local media files
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
