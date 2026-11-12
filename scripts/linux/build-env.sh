#!/usr/bin/env bash
# LIN-06: build environment of the Linux release in an old container (ci.yml, jobs release and cache-release; image
# ubuntu:22.04, glibc 2.35 - the AppImage, deb and rpm then run on Ubuntu 22.04+, Debian 12+, Fedora 36+). Runs as
# root: system packages of the build (APT_PACKAGES of linux.yml plus libssl-dev: the runner image has it, the container does not) and of scripts/linux/package.sh, then
# rustup with stable (on PATH for the next steps of the job via GITHUB_PATH).
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get install -y -q --no-install-recommends \
  ca-certificates curl git build-essential pkg-config python3 binutils \
  libssl-dev libkrb5-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libfontconfig1-dev \
  rpm squashfs-tools desktop-file-utils
if ! command -v rustup > /dev/null; then
  curl -fsSL --retry 3 https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path
fi
if [ -n "${GITHUB_PATH:-}" ]; then echo "${CARGO_HOME:-$HOME/.cargo}/bin" >> "$GITHUB_PATH"; fi
