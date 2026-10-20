# syntax=docker/dockerfile:1

# The web version of hitomi-downloader: the server and its frontend. Nothing of the desktop
# app is in this image, there is no GTK and no WebKit.
#
#   docker build -t hitomi-downloader .
#   docker build --build-arg VERSION=1.2.3 -t hitomi-downloader .   (what a release does)
#
# The Rust stage runs on the platform of the machine that builds and compiles for the platform
# of the image, so an arm64 image is built at native speed and not under emulation.

ARG NODE_VERSION=22
ARG RUST_VERSION=1
ARG DEBIAN_VERSION=trixie


# ---- the version of a release, written by the script the release workflow uses
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_VERSION}-slim AS version
WORKDIR /repo
COPY .github/scripts/set-version.mjs .github/scripts/set-version.mjs
COPY Cargo.toml Cargo.lock package.json ./
COPY src-tauri/tauri.conf.json src-tauri/tauri.conf.json
ARG VERSION=
RUN if [ -n "$VERSION" ]; then node .github/scripts/set-version.mjs "$VERSION"; fi


# ---- the frontend of the web version
FROM --platform=$BUILDPLATFORM node:${NODE_VERSION}-${DEBIAN_VERSION}-slim AS web
WORKDIR /repo
RUN corepack enable
COPY package.json pnpm-lock.yaml ./
RUN --mount=type=cache,target=/root/.local/share/pnpm/store \
    pnpm install --frozen-lockfile
COPY index.html tsconfig.json tsconfig.node.json vite.config.ts uno.config.ts \
     auto-imports.d.ts components.d.ts ./
COPY public public
# the About dialog shows the logo of the desktop app, the favicon and the manifest use its icons
# (`appIcons` in vite.config.ts)
COPY src-tauri/icons/32x32.png src-tauri/icons/64x64.png src-tauri/icons/128x128.png \
     src-tauri/icons/128x128@2x.png src-tauri/icons/icon.png src-tauri/icons/
COPY src src
RUN pnpm build:web


# ---- the server
FROM --platform=$BUILDPLATFORM rust:${RUST_VERSION}-${DEBIAN_VERSION} AS server
ARG TARGETARCH
WORKDIR /repo

# What it takes to compile for the platform of the image: nothing on amd64, a cross compiler and
# the OpenSSL of that platform on arm64
RUN set -eux; \
    case "$TARGETARCH" in \
      amd64) echo x86_64-unknown-linux-gnu > /rust-target ;; \
      arm64) \
        echo aarch64-unknown-linux-gnu > /rust-target; \
        dpkg --add-architecture arm64; \
        apt-get update; \
        apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross libssl-dev:arm64; \
        rm -rf /var/lib/apt/lists/*; \
        rustup target add aarch64-unknown-linux-gnu ;; \
      *) echo "unsupported platform: $TARGETARCH" >&2; exit 1 ;; \
    esac
ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
    CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
    PKG_CONFIG_ALLOW_CROSS=1 \
    PKG_CONFIG_PATH_aarch64_unknown_linux_gnu=/usr/lib/aarch64-linux-gnu/pkgconfig

# The workspace has the desktop app as a member, so its manifest has to be there to load it.
# Its dependencies are not built or downloaded for the server.
COPY --from=version /repo/Cargo.toml /repo/Cargo.lock ./
COPY crates crates
COPY src-tauri src-tauri
# Each platform has caches of its own. The builds of both platforms run at the same time on the
# same machine, and cargo does not expect another cargo in its package caches: two of them
# unpacking the same crate, or fetching the same git dependency, fail with "File exists" or
# "failed to lock file". A cache that is shared between them made the publishing of a release fail.
RUN --mount=type=cache,id=cargo-registry-$TARGETARCH,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cargo-git-$TARGETARCH,target=/usr/local/cargo/git \
    --mount=type=cache,id=cargo-target-$TARGETARCH,target=/repo/target \
    set -eux; \
    cargo build --release --locked -p hitomi-server --target "$(cat /rust-target)"; \
    install -D "target/$(cat /rust-target)/release/hitomi-server" /out/hitomi-server


# ---- the image
FROM debian:${DEBIAN_VERSION}-slim AS runtime

# ca-certificates for the connection to Hitomi, tzdata for TZ. `setpriv`, which gives up root for
# PUID:PGID, is part of util-linux, which every Debian has.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates libssl3t64 tzdata \
 && rm -rf /var/lib/apt/lists/*

COPY --from=server /out/hitomi-server /usr/local/bin/hitomi-server
COPY --from=web /repo/dist-web /app/web
COPY docker/entrypoint.sh /usr/local/bin/entrypoint.sh

# The data directories are not declared as VOLUMEs: an undeclared mount point lets the server
# notice that nothing is mounted (HITOMI_REQUIRE_MOUNT), a declared one would be filled with an
# anonymous volume that is lost unnoticed. Library and export are open to everyone only inside
# the image, so that a named volume, which copies this, can be written by any PUID. A bind mount
# has the permissions of its directory on the host.
RUN mkdir -p /data/config /data/library /data/export \
 && chmod 0777 /data/library /data/export

# All data lies below /data: the config, the library and the export. Where they are is still up to
# the owner, HITOMI_CONFIG_DIR, HITOMI_LIBRARY_DIR and HITOMI_EXPORT_DIR mount them anywhere,
# /data/... is only what they default to.
ENV HITOMI_HOST=0.0.0.0 \
    HITOMI_PORT=8080 \
    HITOMI_CONFIG_DIR=/data/config \
    HITOMI_LIBRARY_DIR=/data/library \
    HITOMI_EXPORT_DIR=/data/export \
    HITOMI_WEB_DIR=/app/web \
    HITOMI_REQUIRE_MOUNT=true \
    PUID=1000 \
    PGID=1000 \
    UMASK=022

EXPOSE 8080

HEALTHCHECK --interval=30s --timeout=6s --start-period=30s --retries=3 \
    CMD ["hitomi-server", "healthcheck"]

ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["hitomi-server"]
