FROM rust:1.83-slim@sha256:540c902e99c384163b688bbd8b5b8520e94e7731b27f7bd0eaa56ae1960627ab

ENV DEBIAN_FRONTEND=noninteractive LANG=C.UTF-8

# python3 runs the example and verifier harnesses; procps provides pkill.
# jq itself is deliberately absent from the image.
RUN apt-get update \
    && apt-get install -y --no-install-recommends python3 procps \
    && rm -rf /var/lib/apt/lists/*

# Non-root agent user. The Rust toolchain stays root-owned and read-only so the
# agent cannot replace the compiler the verifier uses.
RUN useradd --create-home --shell /bin/bash agent \
    && chmod -R go-w /usr/local/cargo /usr/local/rustup \
    && mkdir -p /opt/agent-cargo \
    && chown agent:agent /opt/agent-cargo
ENV CARGO_HOME=/opt/agent-cargo \
    PATH=/usr/local/cargo/bin:$PATH \
    CARGO_NET_OFFLINE=true

WORKDIR /app
COPY workspace/ /app/
RUN chmod +x /app/run_examples.py && chown -R agent:agent /app

# The starting crate must build offline.
RUN su agent -c "cd /app && cargo build --release --offline --quiet" && rm -rf /app/target
