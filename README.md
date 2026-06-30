# ffmpeg-lib-playout-example

Ein bewusst einfaches Beispiel, wie man das OBS-Prinzip ohne FIFO/Pipes/ffmpeg-Prozesse mit FFmpeg-Libraries aufbauen kann.

Dieses Beispiel:

- öffnet eine Playlist aus lokalen Dateien
- dekodiert Video und Audio über `libavformat`/`libavcodec`
- skaliert Video auf ein fixes Zielformat über `libswscale`
- resampelt Audio über `libswresample`
- erzeugt bei fehlendem Video Schwarzbild
- erzeugt bei fehlendem Audio Stille
- gleicht unterschiedlich lange Audio-/Videospuren aus
- normalisiert die Quell-Framerate auf die konfigurierte Ausgabe-FPS
- vergibt eigene durchlaufende PTS-Werte
- encodiert und muxt in eine Ausgabedatei
- kann Video und Audio optional direkt über SDL2 ausgeben

Es ist **kein fertiger 24/7-Playout**, sondern ein Architekturbeispiel.

## Voraussetzungen

Debian/Ubuntu:

```bash
sudo apt install -y pkg-config clang libavformat-dev libavcodec-dev libavutil-dev libswscale-dev libswresample-dev libsdl2-dev
```

Fedora:

```bash
sudo dnf install -y ffmpeg-devel SDL2-devel clang pkgconf-pkg-config
```

## Build

```bash
cargo build
```

Mit Desktop-Ausgabe:

```bash
cargo build --features desktop
```

## Beispiel

```bash
cargo run -- \
  --output out.mp4 \
  input1.mp4 input2.mp4 input3.mp3
```

RTMP wäre konzeptionell ebenfalls über `libavformat` möglich, z.B. mit Output-URL:

```bash
cargo run -- --output rtmp://localhost/live/stream input1.mp4 input2.mp4
```

SDL2-Desktop-Ausgabe:

```bash
cargo run --features desktop -- --desktop input1.mp4 input2.mp4
```

Mit `RUST_LOG=debug` werden Padding und Video-Queue-Unterläufe protokolliert.
