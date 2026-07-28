# syntax=docker/dockerfile:1
#
# Multi-stage Dockerfile for headless-use.
# Stage 1 (builder): compiles the Rust binary.
# Stage 2 (runtime): minimal image with Chromium + the binary.
#
# The runtime image bundles Chromium so `docker run` works without a host
# browser. We install only the shared libraries Chromium needs to keep the
# image small.

FROM rust:1.88-bookworm AS builder
WORKDIR /build
# Cache dependencies
COPY Cargo.toml Cargo.lock* build.rs ./
RUN mkdir -p src && echo 'fn main(){}' > src/main.rs && echo '' > src/lib.rs \
    && cargo build --locked --release || true
COPY src ./src
COPY tests ./tests
RUN cargo build --locked --release

FROM debian:bookworm-slim AS runtime
ARG VERSION=dev
ARG VCS_REF=unknown
LABEL org.opencontainers.image.title="headless-use" \
      org.opencontainers.image.description="Computer use for web development agents, built for headless Linux and CI" \
      org.opencontainers.image.source="https://github.com/flyingsquirrel0419/headless-use" \
      org.opencontainers.image.licenses="Apache-2.0" \
      org.opencontainers.image.version="$VERSION" \
      org.opencontainers.image.revision="$VCS_REF"
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
# No ports are exposed by default: serve/mcp speak stdio. The optional live
# viewer (`view`) listens on 7780; publish it explicitly with -p if used.
ENTRYPOINT ["/usr/bin/tini", "--", "headless-use"]
CMD ["--help"]
