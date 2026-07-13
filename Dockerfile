# syntax=docker/dockerfile:1.7
# Server OCI image: ecaa-workflow-server + harness + CLI + config/lib/ui + agent scripts.
# The harness (spawned by the server) runs scripts/agent-claude.sh, which launches the bio-min
# TASK container as a SIBLING over the mounted runtime socket (DooD). The same-absolute-path
# mount convention in agent-claude.sh neutralizes the DooD path-translation trap.

########## UI build ##########
FROM node:22-slim@sha256:813a7480f28fdadac1f7f5c824bcdad435b5bc1322a5968bbbdef8d058f9dff4 AS ui
WORKDIR /ui
COPY ui/package.json ui/package-lock.json ./
RUN npm ci
COPY ui/ ./
RUN npm run build

########## Rust builder (honors rust-toolchain.toml: channel 1.93) ##########
FROM rust:1.93-slim@sha256:c0a38f5662afdb298898da1d70b909af4bda4e0acff2dc52aea6360a9b9c6956 AS chef
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends \
      pkg-config libssl-dev git \
 && rm -rf /var/lib/apt/lists/* \
 && cargo install cargo-chef --locked

FROM chef AS planner
COPY . .
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /app/recipe.json recipe.json
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo chef cook --release --recipe-path recipe.json
COPY . .
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release \
      --bin ecaa-workflow-server \
      --bin ecaa-workflow-harness \
      --bin ecaa-workflow \
      --bin ecaa-workflow-audit-proof \
 && mkdir -p /out \
 && cp target/release/ecaa-workflow-server \
       target/release/ecaa-workflow-harness \
       target/release/ecaa-workflow \
       target/release/ecaa-workflow-audit-proof /out/

########## Runtime ##########
FROM debian:trixie-slim@sha256:28de0877c2189802884ccd20f15ee41c203573bd87bb6b883f5f46362d24c5c2 AS runtime
ARG TARGETARCH
ARG DOCKER_CLI_VERSION=27.3.1
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git jq python3 python3-venv libssl3 nodejs npm \
 && rm -rf /var/lib/apt/lists/*
# WRROC substrate validator (audit-proof Invariant 6). `runcrate` >= 0.5 in an
# isolated venv, exposed as /usr/local/bin/runcrate so scripts/wrroc-validate.py
# (system python3, stdlib-only) can shell `runcrate report`. Enables
# substrate_validity to verify at export time instead of staying `unverified`.
RUN set -eux; \
    python3 -m venv /opt/validator-venv; \
    /opt/validator-venv/bin/pip install --no-cache-dir "runcrate>=0.5.0"; \
    ln -s /opt/validator-venv/bin/runcrate /usr/local/bin/runcrate; \
    test -x /opt/validator-venv/bin/runcrate
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) A=x86_64 ;; \
      arm64) A=aarch64 ;; \
      "" ) A="$(uname -m)" ;; \
      *) echo "unsupported arch: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    curl -fsSL "https://download.docker.com/linux/static/stable/${A}/docker-${DOCKER_CLI_VERSION}.tgz" \
      | tar -xz -C /usr/local/bin --strip-components=1 docker/docker; \
    docker --version
WORKDIR /app
COPY --from=builder /out/ecaa-workflow-server      /usr/local/bin/
COPY --from=builder /out/ecaa-workflow-harness     /usr/local/bin/
COPY --from=builder /out/ecaa-workflow             /usr/local/bin/
COPY --from=builder /out/ecaa-workflow-audit-proof /usr/local/bin/
COPY config/  /app/config/
COPY lib/     /app/lib/
COPY scripts/ /app/scripts/
COPY --from=ui /ui/dist /app/ui/dist
ENV ECAA_CONFIG_DIR=/app/config
EXPOSE 3000
HEALTHCHECK --interval=30s --timeout=3s --start-period=30s --retries=3 \
  CMD curl -fsS http://127.0.0.1:3000/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/ecaa-workflow-server"]
