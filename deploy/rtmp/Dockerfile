# syntax=docker/dockerfile:1
#
# One image that does RTMP ingest + recording (MediaMTX) and can bridge each
# stream into MoQ (ffmpeg + moq-cli). Single glibc base so the moq-cli binary
# we build here runs as-is.
#
# Build context is the repo root (see docker-compose.yml).

# 1) Build a stock moq-cli (used only to publish the live bridge into the relay).
FROM rust:1.85-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p moq-cli && cp target/release/moq-cli /moq-cli

# 2) MediaMTX + ffmpeg + moq-cli.
FROM debian:bookworm-slim

# Bump as new MediaMTX releases land: https://github.com/bluenviron/mediamtx/releases
ARG MEDIAMTX_VERSION=1.9.3
ARG TARGETARCH=amd64

RUN apt-get update \
 && apt-get install -y --no-install-recommends ffmpeg ca-certificates curl \
 && curl -fsSL "https://github.com/bluenviron/mediamtx/releases/download/v${MEDIAMTX_VERSION}/mediamtx_v${MEDIAMTX_VERSION}_linux_${TARGETARCH}.tar.gz" \
    | tar -xz -C /usr/local/bin mediamtx \
 && apt-get purge -y curl && apt-get autoremove -y \
 && rm -rf /var/lib/apt/lists/*

COPY --from=build /moq-cli /usr/local/bin/moq-cli
COPY deploy/rtmp/mediamtx.yml /etc/mediamtx.yml

# RTMP in, RTSP (internal bridge), HTTP API.
EXPOSE 1935 8554 9997

ENTRYPOINT ["mediamtx", "/etc/mediamtx.yml"]
