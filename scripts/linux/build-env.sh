#!/usr/bin/env bash
# LIN-06: build environment of the Linux release in an old container (ci.yml, jobs release and cache-release; image
# almalinux:9, glibc 2.34 - the AppImage, deb and rpm then run on RHEL/Alma/Rocky 9, Ubuntu 22.04+, Debian 12+,
# Fedora 35+). Runs as root: the build packages (APT_PACKAGES of linux.yml and what the runner image has on top:
# OpenSSL headers, clang for bindgen, cmake), the packaging tools of scripts/linux/package.sh (dpkg-deb from EPEL),
# then rustup with stable (on PATH for the next steps of the job via GITHUB_PATH). apt (Debian, Ubuntu) works too.
set -euo pipefail
if command -v dnf > /dev/null; then
  dnf -y -q install epel-release dnf-plugins-core
  dnf config-manager --set-enabled crb
  dnf -y -q install --setopt=install_weak_deps=False \
    git gcc gcc-c++ make pkgconf-pkg-config python3 binutils findutils clang clang-devel cmake \
    openssl-devel krb5-devel libxkbcommon-devel libxkbcommon-x11-devel wayland-devel libxcb-devel \
    fontconfig-devel freetype-devel rpm-build squashfs-tools desktop-file-utils dpkg
else
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -q
  apt-get install -y -q --no-install-recommends \
    ca-certificates curl git build-essential pkg-config python3 binutils clang libclang-dev cmake \
    libssl-dev libkrb5-dev libxkbcommon-dev libxkbcommon-x11-dev libwayland-dev libxcb1-dev libfontconfig1-dev \
    rpm squashfs-tools desktop-file-utils
fi
if ! command -v rustup > /dev/null; then
  curl -fsSL --retry 3 https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable --no-modify-path
fi
if [ -n "${GITHUB_PATH:-}" ]; then echo "${CARGO_HOME:-$HOME/.cargo}/bin" >> "$GITHUB_PATH"; fi
