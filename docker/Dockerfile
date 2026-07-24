# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile for headless-use.
# Stage 1 (builder): compiles the Rust binary.
# Stage 2 (runtime): minimal image with Chromium + the binary.
#
# The runtime image bundles Chromium so `docker run` works without a host
# browser. We install only the shared libraries Chromium needs to keep the
# image small.

FROM rust:1-bookworm AS builder
WORKDIR /build
# Cache dependencies
COPY Cargo.toml Cargo.lock* build.rs ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --release || true
COPY src ./src
COPY tests ./tests
RUN cargo build --release

FROM debian:bookworm-slim AS runtime
# Chromium runtime deps + fonts (including CJK for screenshot fidelity).
RUN apt-get update && apt-get install -y --no-install-recommends \
    chromium \
    fonts-noto-cjk \
    fonts-liberation \
    ca-certificates \
    tini \
    && rm -rf /var/lib/apt/lists/*

# Non-root user; Chromium refuses to run as root without --no-sandbox, and we
# prefer not to disable the sandbox by default.
RUN useradd -m -u 1000 hu
WORKDIR /home/hu
COPY --from=builder /build/target/release/headless-use /usr/local/bin/headless-use

USER hu
ENV HEADLESS_USE_BROWSER_PATH=/usr/bin/chromium
EXPOSE 0
ENTRYPOINT ["/usr/bin/tini", "--", "headless-use"]
CMD ["--help"]
