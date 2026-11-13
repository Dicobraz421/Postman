#!/usr/bin/env bash
# LIN-01, LIN-06: Linux release files (x86_64, docs/decisions/2026-11-12-linux.md; no Flatpak):
#   postman-<version>-linux-x64.AppImage - one file for any distribution (the self-update replaces it);
#   postman-<version>-linux-x64.deb      - Debian, Ubuntu and derivatives;
#   postman-<version>-linux-x64.rpm      - Fedora, openSUSE, RHEL and derivatives.
# Each holds postman, postman-worker and postman-viewer side by side (the worker looks for them next to postman),
# postman.desktop (the mailto scheme), hicolor icons from app-gpui/res/postman.ico. deb and rpm also hold an AppArmor
# profile for the worker and the viewer (data/apparmor/postman): Ubuntu 23.10+ forbids unprivileged user namespaces,
# and without them the worker sandbox runs without namespaces (ADR 0010, mode optional). The profile goes to
# /etc/apparmor.d only where AppArmor 4 is installed; the AppImage has none - the sandbox diagnostics show it.
# Glibc: the binaries take the glibc of the build host - CI builds in an old container (ci.yml, job release); the
# minimum version goes into the package dependencies by itself (the newest GLIBC_x.y symbol the binaries use).
# Takes the binaries of a finished build (cargo build --release --workspace --features postman-worker/viewer).
# Needs dpkg-deb, rpmbuild, mksquashfs and curl (the AppImage runtime: AppImage/type2-runtime of a fixed release, or the file in
# APPIMAGE_RUNTIME). Prints the files; in GitHub Actions also sets the outputs name, dir (the folder with the files),
# assets (the same folder: release files, docs/DEV.md "Выпуск") and version.
# Usage: scripts/linux/package.sh [--target-dir <cargo target dir>] [--profile release|debug] [--out <dir, default dist>]
set -euo pipefail

root="$(cd "$(dirname "$0")/../.." && pwd)"
here="$root/scripts/linux"
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
bins=(postman postman-worker postman-viewer)
for b in "${bins[@]}"; do
  if [ ! -x "$bin/$b" ]; then
    echo "no $bin/$b - run cargo build --$profile --workspace --features postman-worker/viewer first" >&2
    exit 1
  fi
done
for t in dpkg-deb rpmbuild mksquashfs curl python3; do
  command -v "$t" > /dev/null || { echo "no $t (scripts/linux/package.sh: header)" >&2; exit 1; }
done

# Version - [workspace.package] of the root Cargo.toml (the same the binaries are built with).
version="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)"
if [ -z "$version" ]; then echo "no version in [workspace.package] of Cargo.toml" >&2; exit 1; fi
# deb and rpm: a pre-release sorts before the release with "~" (0.1.0~beta.4 < 0.1.0).
pkgver="${version/-/\~}"

# Oldest glibc the binaries run on: the newest versioned symbol they import.
glibc="$(for b in "${bins[@]}"; do objdump -T "$bin/$b"; done | grep -o 'GLIBC_[0-9.]*' | sed 's/GLIBC_//' | sort -uV | tail -n1)"
if [ -z "$glibc" ]; then echo "no GLIBC_ symbols in the binaries" >&2; exit 1; fi

# Libraries: linked ones must be known here (a new one - a line below and in both dependency lists); gpui and wgpu
# load Vulkan, xkbcommon-x11 and the X11/Wayland client at run time (dlopen), so they are listed by hand.
for b in "${bins[@]}"; do objdump -p "$bin/$b" | sed -n 's/^ *NEEDED *//p'; done | sort -u | while read -r so; do
  case "$so" in
    libc.so.6|libm.so.6|libdl.so.2|libpthread.so.0|librt.so.1|libgcc_s.so.1|ld-linux-x86-64.so.2) ;;
    libssl.so.3|libcrypto.so.3|libgssapi_krb5.so.2|libxkbcommon.so.0|libxkbcommon-x11.so.0|libfontconfig.so.1|libfreetype.so.6) ;;
    libxcb.so.1|libX11.so.6|libX11-xcb.so.1|libwayland-client.so.0|libvulkan.so.1) ;;
    *) echo "unknown linked library $so: add it to scripts/linux/package.sh (deb Depends, rpm Requires)" >&2; exit 1 ;;
  esac
done
deb_depends="libc6 (>= $glibc), libgcc-s1, libssl3 | libssl3t64, libgssapi-krb5-2, libxkbcommon0, libxkbcommon-x11-0, libfontconfig1, libfreetype6, libxcb1, libx11-6, libx11-xcb1, libwayland-client0, libvulkan1, mesa-vulkan-drivers | vulkan-icd, xdg-utils"
rpm_requires=(
  "libc.so.6(GLIBC_$glibc)(64bit)" "libssl.so.3()(64bit)" "libcrypto.so.3()(64bit)" "libgssapi_krb5.so.2()(64bit)" "libxkbcommon.so.0()(64bit)"
  "libxkbcommon-x11.so.0()(64bit)" "libfontconfig.so.1()(64bit)" "libfreetype.so.6()(64bit)" "libxcb.so.1()(64bit)"
  "libX11.so.6()(64bit)" "libX11-xcb.so.1()(64bit)" "libwayland-client.so.0()(64bit)" "libvulkan.so.1()(64bit)"
  "xdg-utils"
)

name="postman-$version-linux-x64"
assets="$out/$name"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
rm -rf "$assets"
mkdir -p "$assets"

# The tree under /usr: binaries, desktop file, icons, AppArmor profile (deb and rpm: copied to /etc by the scripts).
tree="$work/tree"
install -d "$tree/usr/bin" "$tree/usr/share/applications" "$tree/usr/share/postman/apparmor"
install -m 755 "${bins[@]/#/$bin/}" "$tree/usr/bin/"
install -m 644 "$here/data/postman.desktop" "$tree/usr/share/applications/postman.desktop"
install -m 644 "$here/data/apparmor/postman" "$tree/usr/share/postman/apparmor/postman"
if command -v desktop-file-validate > /dev/null; then desktop-file-validate "$tree/usr/share/applications/postman.desktop"; fi
# Icons: the PNG frames of postman.ico (16, 32, 64, 256) -> hicolor/<n>x<n>/apps/postman.png.
python3 - "$root/app-gpui/res/postman.ico" "$tree/usr/share/icons/hicolor" <<'PY'
import os, struct, sys
data = open(sys.argv[1], "rb").read()
count = struct.unpack("<HHH", data[:6])[2]
frames = {}
for i in range(count):
    w, h, _, _, _, _, size, off = struct.unpack("<BBBBHHII", data[6 + 16 * i:22 + 16 * i])
    png = data[off:off + size]
    if png[:4] == b"\x89PNG":
        frames[w or 256] = png
for px in (16, 32, 64, 256):
    if px not in frames:
        sys.exit(f"postman.ico has no {px}x{px} PNG frame")
    d = f"{sys.argv[2]}/{px}x{px}/apps"
    os.makedirs(d, exist_ok=True)
    with open(f"{d}/postman.png", "wb") as f:
        f.write(frames[px])
PY
find "$tree" -type d -exec chmod 755 {} +
find "$tree/usr/share" -type f -exec chmod 644 {} +

# --- deb ---
deb="$assets/$name.deb"
debroot="$work/deb"
cp -a "$tree" "$debroot"
install -d "$debroot/DEBIAN"
size="$(du -sk --exclude=DEBIAN "$debroot" | cut -f1)"
cat > "$debroot/DEBIAN/control" <<CONTROL
Package: postman
Version: $pkgver
Architecture: amd64
Maintainer: Anton Antropov <https://github.com/Dicobraz421/Postman/issues>
Installed-Size: $size
Depends: $deb_depends
Section: mail
Priority: optional
Homepage: https://github.com/Dicobraz421/Postman
Description: Mail, calendar and contacts client
 Postman works with The Post, Exchange (EWS), JMAP, IMAP, CalDAV and CardDAV.
 Attachments and messages are opened in a sandboxed worker process.
CONTROL
install -m 755 "$here/data/apparmor.sh" "$debroot/DEBIAN/postinst"
install -m 755 "$here/data/apparmor.sh" "$debroot/DEBIAN/prerm"
# Owner and times are not part of the package: the same build gives the same deb.
find "$debroot" -exec touch -h -d '2000-01-01 00:00Z' {} +
dpkg-deb --root-owner-group -Zxz --build "$debroot" "$deb" > /dev/null

# --- rpm ---
rpm="$assets/$name.rpm"
rpmtop="$work/rpm"
mkdir -p "$rpmtop/SPECS" "$rpmtop/RPMS"
{
  echo "Name: postman"
  echo "Version: ${pkgver}"
  echo "Release: 1"
  echo "Summary: Mail, calendar and contacts client"
  echo "License: Proprietary"
  echo "URL: https://github.com/Dicobraz421/Postman"
  echo "BuildArch: x86_64"
  echo "AutoReqProv: no"
  for r in "${rpm_requires[@]}"; do echo "Requires: $r"; done
  echo "Recommends: mesa-vulkan-drivers"
  echo "%global debug_package %{nil}"
  echo "%global __os_install_post %{nil}"
  echo "%global _build_id_links none"
  echo "%description"
  echo "Postman works with The Post, Exchange (EWS), JMAP, IMAP, CalDAV and CardDAV."
  echo "Attachments and messages are opened in a sandboxed worker process."
  echo "%install"
  echo "cp -a '$tree/.' %{buildroot}/"
  echo "%post"
  echo "set -- configure"
  cat "$here/data/apparmor.sh"
  echo "%preun"
  echo 'if [ "$1" = 0 ]; then set -- remove; else set -- upgrade; fi'
  cat "$here/data/apparmor.sh"
  echo "%files"
  echo "/usr/bin/postman"
  echo "/usr/bin/postman-worker"
  echo "/usr/bin/postman-viewer"
  echo "/usr/share/applications/postman.desktop"
  echo "/usr/share/icons/hicolor/*/apps/postman.png"
  echo "/usr/share/postman"
} > "$rpmtop/SPECS/postman.spec"
rpmbuild -bb --quiet --define "_topdir $rpmtop" --define "_rpmdir $rpmtop/RPMS" \
  --define "_build_name_fmt %%{NAME}.rpm" "$rpmtop/SPECS/postman.spec"
mv "$rpmtop/RPMS/postman.rpm" "$rpm"

# --- AppImage: type 2 runtime + squashfs of AppDir (AppRun, desktop file and icon at the root, usr/ inside) ---
appimage="$assets/$name.AppImage"
appdir="$work/AppDir"
mkdir -p "$appdir"
cp -a "$tree/usr" "$appdir/usr"
rm -rf "$appdir/usr/share/postman"
install -m 755 "$here/data/AppRun" "$appdir/AppRun"
cp "$tree/usr/share/applications/postman.desktop" "$appdir/postman.desktop"
cp "$tree/usr/share/icons/hicolor/256x256/apps/postman.png" "$appdir/postman.png"
ln -s postman.png "$appdir/.DirIcon"
# Runtime: a fixed release of AppImage/type2-runtime, checked by SHA-256 (a new release - both lines here). A local
# file in APPIMAGE_RUNTIME (offline builds) is checked the same way.
runtime_tag="20251108"
runtime_sha256="2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d"
runtime="${APPIMAGE_RUNTIME:-}"
if [ -z "$runtime" ]; then
  runtime="$work/runtime"
  curl -fsSL --retry 3 -o "$runtime" "https://github.com/AppImage/type2-runtime/releases/download/$runtime_tag/runtime-x86_64"
fi
echo "$runtime_sha256  $runtime" | sha256sum -c --quiet - || { echo "AppImage runtime: SHA-256 mismatch" >&2; exit 1; }
find "$appdir" -exec touch -h -d '2000-01-01 00:00Z' {} +
# zstd where mksquashfs has it, xz otherwise (both read by the runtime); times from SOURCE_DATE_EPOCH (squashfs-tools 4.4+).
comp=zstd
mkhelp="$(mksquashfs -help 2>&1 || true)"
case "$mkhelp" in *zstd*) ;; *) comp=xz ;; esac
SOURCE_DATE_EPOCH=946684800 mksquashfs "$appdir" "$work/app.squashfs" -root-owned -noappend -comp "$comp" > /dev/null
cat "$runtime" "$work/app.squashfs" > "$appimage"
chmod 755 "$appimage"

ls -1 "$assets"/*
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "name=$name"
    echo "dir=$assets"
    echo "assets=$assets"
    echo "version=$version"
  } >> "$GITHUB_OUTPUT"
fi
