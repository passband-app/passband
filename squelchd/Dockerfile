# Builds squelchd — the self-hosted daemon (Gmail sync loop + agent door /mcp +
# human door /client). Build context must be the workspace root:
#
#   docker build -f squelchd/Dockerfile .
#
# Multi-arch: the builder always runs on the CI runner's native arch
# (BUILDPLATFORM) and cross-compiles when TARGETPLATFORM differs, so a plain
# amd64 runner produces the arm64 image without QEMU. The only cross needs are
# the Debian cross g++ (rusqlite compiles bundled SQLite; ort links a static
# onnxruntime that wants libstdc++) and the matching cargo linker env.

# Trixie, not bookworm: ort's prebuilt static onnxruntime is compiled against
# glibc >= 2.38 (__isoc23_strtol and friends); bookworm's 2.36 can't link it.
FROM --platform=$BUILDPLATFORM rust:1-slim-trixie@sha256:1a939748a7cbab172de91a014e3804a97af1394a8c844faa67fce9401c4e40fb AS chef
ARG BUILDPLATFORM
ARG TARGETPLATFORM
WORKDIR /build

# cargo-chef, so the dependency compile (the ONNX tree, most of this build) is
# its own layer keyed on the dependency graph alone. A plain `COPY Cargo.toml
# Cargo.lock` + `cargo build` cannot cache it across releases: every daemon tag
# bumps the workspace version, which rewrites both files. chef's recipe masks
# the workspace crates' own versions, so a release that changes no dependency
# reuses the cooked layer from the registry cache and compiles only our crates.
# Installed before the toolchain COPY so a toolchain bump does not reinstall it.
RUN cargo install cargo-chef --locked --version 0.1.78

# The compiler is rust-toolchain.toml's, not the image's; see that file's
# header for why, and for what a bump of it reaches. Before the platform RUN
# below on purpose: nothing here reads TARGETPLATFORM, so the multi-arch build
# shares this layer between its two platforms instead of downloading the
# toolchain twice, and the target is added to this toolchain, the active one
# in a directory that holds the toml.
COPY rust-toolchain.toml ./
RUN rustup toolchain install --no-self-update

RUN set -eux; \
    case "$TARGETPLATFORM" in \
      linux/amd64) echo x86_64-unknown-linux-gnu > /rust-triple ;; \
      linux/arm64) echo aarch64-unknown-linux-gnu > /rust-triple ;; \
      *) echo "unsupported TARGETPLATFORM: $TARGETPLATFORM" >&2; exit 1 ;; \
    esac; \
    triple="$(cat /rust-triple)"; \
    # ort links -lstdc++ even natively, and rust:slim ships gcc without g++;
    # the cross package additionally brings the target-arch libstdc++.
    pkgs="g++"; \
    if [ "$BUILDPLATFORM" != "$TARGETPLATFORM" ]; then \
      prefix="$(echo "$triple" | sed 's/-unknown//')"; \
      pkgs="$pkgs g++-$(echo "$prefix" | tr '_' '-')"; \
    fi; \
    apt-get update; \
    apt-get install -y --no-install-recommends $pkgs; \
    rm -rf /var/lib/apt/lists/*; \
    rustup target add "$triple"; \
    # The cross linker env, written once and sourced by both cargo steps below:
    # chef's cooked dependencies are reused only if the final build sees the
    # same target, profile and compilers they were built with.
    : > /cross-env; \
    if [ "$BUILDPLATFORM" != "$TARGETPLATFORM" ]; then \
      lower="$(echo "$triple" | tr '-' '_')"; \
      upper="$(echo "$lower" | tr '[:lower:]' '[:upper:]')"; \
      { echo "export CARGO_TARGET_${upper}_LINKER=${prefix}-gcc"; \
        echo "export CC_${lower}=${prefix}-gcc"; \
        echo "export CXX_${lower}=${prefix}-g++"; } > /cross-env; \
    fi

FROM chef AS planner
COPY Cargo.toml Cargo.lock ./
COPY squelch-core ./squelch-core
COPY squelch-httpauth ./squelch-httpauth
COPY squelch-ratelimit ./squelch-ratelimit
COPY squelch-mcp ./squelch-mcp
COPY squelch-tui ./squelch-tui
COPY squelch-api ./squelch-api
COPY squelch-relay ./squelch-relay
COPY squelch-broker ./squelch-broker
# Hosted control plane + VPS provisioner. Neither ships in THIS image, but both
# are workspace members and cargo refuses to resolve the workspace without them.
COPY squelch-control ./squelch-control
COPY squelch-warden ./squelch-warden
COPY squelchd ./squelchd
COPY squelch-embed ./squelch-embed
RUN cargo chef prepare --recipe-path recipe.json

FROM chef AS builder
COPY --from=planner /build/recipe.json recipe.json
RUN set -eux; . /cross-env; \
    cargo chef cook --release --locked -p squelchd --target "$(cat /rust-triple)" \
      --recipe-path recipe.json
COPY --from=planner /build ./
# The target dir is deleted in the same layer, so the layer the registry cache
# re-uploads every tag is the binary, not gigabytes of build tree.
RUN set -eux; . /cross-env; triple="$(cat /rust-triple)"; \
    cargo build --release --locked -p squelchd --target "$triple"; \
    cp "target/$triple/release/squelchd" /squelchd; \
    rm -rf target

FROM debian:trixie-slim@sha256:62bd423aeee912639ca8ee681c2b762cb3e25137e0a92b20be455898a9f034ad
# rustls everywhere — the only runtime need is CA roots for Google's APIs (and
# the one-time fastembed weights download). Trixie to match the builder's glibc.
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --home-dir /data --no-create-home squelch

COPY --from=builder /squelchd /usr/local/bin/squelchd

# All state lives under one mount: sqlite db, file-backend credentials, and
# (via HOME) the fastembed ONNX weights cache at /data/.local/share/squelch/models.
# Owned here for the no-volume case; a mounted volume arrives root-owned and
# overlays this, which the entrypoint fixes before dropping privileges.
RUN mkdir -p /data && chown squelch:squelch /data

# In a container, loopback-only would make the published port unreachable —
# bind all interfaces and let the operator control exposure with -p / a proxy.
# Everything else matches deploy/DEPLOY.md; see deploy/DOCKER.md for the
# required vars (SQUELCH_API_TOKEN, SQUELCH_ACCOUNT_EMAIL, SQUELCH_CLIENT_ID/
# _SECRET, and SQUELCH_MCP_ALLOWED_HOSTS when fronted by a proxy).
ENV SQUELCH_BIND=0.0.0.0:8848 \
    SQUELCH_CRED_BACKEND=file \
    SQUELCH_DB_PATH=/data/squelch.db \
    SQUELCH_CREDENTIALS_PATH=/data/credentials.json \
    HOME=/data

EXPOSE 8848

COPY squelchd/docker-entrypoint.sh /usr/local/bin/docker-entrypoint.sh
RUN chmod +x /usr/local/bin/docker-entrypoint.sh

# Still root here: the entrypoint chowns the mounted volume, then execs
# squelchd as `squelch` via setpriv. The daemon itself never runs privileged.
ENTRYPOINT ["/usr/local/bin/docker-entrypoint.sh"]
CMD ["serve"]
