FROM rust:1-trixie

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        clang \
        libavcodec-dev \
        libavdevice-dev \
        libavfilter-dev \
        libavformat-dev \
        libavutil-dev \
        libclang-dev \
        libswresample-dev \
        libswscale-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY src ./src

CMD ["sh", "-c", "cargo build --release --no-default-features && mkdir -p /artifacts && cp target/release/rust-playout-example /artifacts/rust-playout-example"]
