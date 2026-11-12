#!/usr/bin/env bash
# LIN-01: Linux build archive (x86_64) for CI - not a release (AppImage, deb and rpm - LIN-06,
# docs/decisions/2026-11-12-linux.md). The folder postman-<version>-linux-x64 holds postman, postman-worker and
# postman-viewer (the worker looks for them next to postman); the archive postman-<version>-linux-x64.tar.gz holds it.
# Takes the binaries of a finished build (cargo build --release --workspace --features postman-worker/viewer).
# Prints the archive path; in GitHub Actions also sets the outputs name, dir, tar and version.
# Usage: scripts/linux/package.sh [--target-dir <cargo target dir>] [--profile release|debug] [--out <dir, default dist>]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
target=""
profile="release"
out="$root/dist"
while [ $# -gt 0 ]; do
  case "$1" in
    --target-dir) target="$2"; shift 2 ;;
    --profile) profile="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
# Build directory from cargo (honours CARGO_TARGET_DIR) - never a hard-coded target/ (H-01).
if [ -z "$target" ]; then
  target="$(cargo metadata --format-version 1 --no-deps --manifest-path "$root/Cargo.toml" | python3 -c 'import json,sys; print(json.load(sys.stdin)["target_directory"])')"
fi
mkdir -p "$out"
target="$(cd "$target" && pwd)"
out="$(cd "$out" && pwd)"
bin="$target/$profile"
for b in postman postman-worker postman-viewer; do
  if [ ! -x "$bin/$b" ]; then
    echo "no $bin/$b - run cargo build --$profile --workspace --features postman-worker/viewer first" >&2
    exit 1
  fi
done

# Version - [workspace.package] of the root Cargo.toml (the same the binaries are built with).
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)"
if [ -z "$version" ]; then echo "no version in [workspace.package] of Cargo.toml" >&2; exit 1; fi

name="postman-$version-linux-x64"
stage="$out/$name"
tar="$out/$name.tar.gz"
rm -rf "$stage" "$tar"
mkdir -p "$stage"
install -m 755 "$bin/postman" "$bin/postman-worker" "$bin/postman-viewer" "$stage/"
# Owner and times are not part of the archive: the same build gives the same tar.gz.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime='2000-01-01 00:00Z' -C "$out" -czf "$tar" "$name"

echo "$tar"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "name=$name"
    echo "dir=$stage"
    echo "tar=$tar"
    echo "version=$version"
  } >> "$GITHUB_OUTPUT"
fi
