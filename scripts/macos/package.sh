#!/usr/bin/env bash
# MAC-01, MAC-06: Postman.app for macOS (Apple Silicon, macOS 13+), the zip postman-<version>-macos-arm64.zip
# (Postman.app and README.txt - the self-update replaces the bundle from it) and the disk image
# postman-<version>-macos-arm64.dmg (Postman.app, README.txt and the "Applications" shortcut to drag it onto).
# Contents/MacOS: postman, postman-worker, postman-viewer (the worker looks for them next to postman);
# Info.plist: bundle id, version from Cargo.toml, LSMinimumSystemVersion 13.0, the mailto scheme; Postman.icns made
# from app-gpui/res/postman.ico (PNG frames).
# Signature: ad-hoc (codesign -s -) by default - no Apple Developer ID yet (docs/decisions/2026-11-11-macos.md), the
# user opens it via "Privacy & Security -> Open Anyway". With MACOS_SIGN_IDENTITY ("Developer ID Application: ...",
# in an unlocked keychain) - hardened runtime and a timestamp; with APPLE_ID, APPLE_TEAM_ID and APPLE_APP_PASSWORD too -
# notarization (notarytool) and a stapled ticket on the app and the dmg. Both are off until the owner has the account.
# Takes the binaries of a finished build (cargo build --release --workspace --features postman-worker/viewer).
# Prints the zip and dmg paths; in GitHub Actions also sets the outputs name, app, zip, dmg, dir and assets (the folder
# with the zip and the dmg: the build artifact and the release files, docs/DEV.md "Выпуск") and version.
# Usage: scripts/macos/package.sh [--target-dir <cargo target dir>] [--profile release|debug] [--out <dir, default dist>]
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
# CFBundleVersion - integers only: a.b.(c*1000 + r), as the MSI (tools/pack/src/ids.rs): alpha.N -> N, beta.N -> 300+N,
# rc.N -> 600+N, stable -> 999. 0.1.0-beta.4 -> 0.1.304.
core="${version%%-*}"
pre=""
if [ "$core" != "$version" ]; then pre="${version#*-}"; fi
IFS=. read -r major minor patch <<< "$core"
case "$pre" in
  "") r=999 ;;
  alpha.*) r=$((${pre#alpha.})) ;;
  beta.*) r=$((300 + ${pre#beta.})) ;;
  rc.*) r=$((600 + ${pre#rc.})) ;;
  *) echo "version $version: pre-release must be alpha.N, beta.N or rc.N" >&2; exit 1 ;;
esac
build="$major.$minor.$((patch * 1000 + r))"

name="postman-$version-macos-arm64"
stage="$out/$name"
app="$stage/Postman.app"
assets="$out/assets"
zip="$assets/$name.zip"
dmg="$assets/$name.dmg"
rm -rf "$stage" "$assets"
mkdir -p "$assets"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin/postman" "$bin/postman-worker" "$bin/postman-viewer" "$app/Contents/MacOS/"

# Icon: the PNG frames of postman.ico -> iconset -> icns (iconutil takes a partial set; 512 and 1024 are absent).
iconset="$(mktemp -d)/Postman.iconset"
mkdir -p "$iconset"
python3 - "$root/app-gpui/res/postman.ico" "$iconset" <<'PY'
import struct, sys
data = open(sys.argv[1], "rb").read()
count = struct.unpack("<HHH", data[:6])[2]
frames = {}
for i in range(count):
    w, h, _, _, _, _, size, off = struct.unpack("<BBBBHHII", data[6 + 16 * i:22 + 16 * i])
    png = data[off:off + size]
    if png[:4] == b"\x89PNG":
        frames[w or 256] = png
names = {16: ["icon_16x16"], 32: ["icon_16x16@2x", "icon_32x32"], 64: ["icon_32x32@2x"], 256: ["icon_128x128@2x", "icon_256x256"]}
for px, files in names.items():
    if px not in frames:
        sys.exit(f"postman.ico has no {px}x{px} PNG frame")
    for f in files:
        open(f"{sys.argv[2]}/{f}.png", "wb").write(frames[px])
PY
sips -z 128 128 "$iconset/icon_256x256.png" --out "$iconset/icon_128x128.png" > /dev/null
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Postman.icns"
rm -rf "$(dirname "$iconset")"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key><string>en</string>
  <key>CFBundleDisplayName</key><string>Postman</string>
  <key>CFBundleExecutable</key><string>postman</string>
  <key>CFBundleIconFile</key><string>Postman</string>
  <key>CFBundleIdentifier</key><string>io.github.dicobraz421.postman</string>
  <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
  <key>CFBundleName</key><string>Postman</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$build</string>
  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key><string>Email Address URL</string>
      <key>CFBundleURLSchemes</key><array><string>mailto</string></array>
    </dict>
  </array>
  <key>LSApplicationCategoryType</key><string>public.app-category.productivity</string>
  <key>LSArchitecturePriority</key><array><string>arm64</string></array>
  <key>LSMinimumSystemVersion</key><string>13.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSHumanReadableCopyright</key><string>Copyright (c) 2026 Anton Antropov. All rights reserved.</string>
</dict>
</plist>
PLIST
plutil -lint "$app/Contents/Info.plist" > /dev/null

identity="${MACOS_SIGN_IDENTITY:-}"
notarize=""
if [ -n "$identity" ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ] && [ -n "${APPLE_APP_PASSWORD:-}" ]; then
  notarize=1
fi

# Signature of the whole bundle: helpers first, then the app (the bundle seal covers Info.plist and Resources).
# Developer ID - hardened runtime (no entitlements needed: no JIT, the WASM viewers are interpreted) and a timestamp.
if [ -n "$identity" ]; then
  sign=(codesign --force --options runtime --timestamp --sign "$identity")
else
  sign=(codesign --force --sign -)
fi
"${sign[@]}" "$app/Contents/MacOS/postman-worker" "$app/Contents/MacOS/postman-viewer"
"${sign[@]}" "$app"
codesign --verify --deep --strict "$app"

# Notarization: the service checks the upload and issues a ticket; stapler puts it into the file, so Gatekeeper does
# not need the network on the first launch. Apple ID with an app-specific password (appleid.apple.com).
notarize_file() {
  xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" --password "$APPLE_APP_PASSWORD" \
    --wait --timeout 30m
}
if [ -n "$notarize" ]; then
  upload="$(mktemp -d)/Postman.zip"
  ditto -c -k --keepParent "$app" "$upload"
  notarize_file "$upload"
  rm -rf "$(dirname "$upload")"
  xcrun stapler staple "$app"
fi

cp "$root/scripts/macos/README.txt" "$stage/README.txt"
# ditto keeps the bundle as Finder does (extended attributes, the signature); the zip holds Postman.app and README.txt.
ditto -c -k --sequesterRsrc "$stage" "$zip"

# Disk image: Postman.app, README.txt and a shortcut to /Applications - the user drags the app onto it. hdiutil on
# CI runners sometimes fails with "Resource busy" - three attempts.
dmgsrc="$(mktemp -d)/Postman"
mkdir -p "$dmgsrc"
ditto "$app" "$dmgsrc/Postman.app"
cp "$stage/README.txt" "$dmgsrc/README.txt"
ln -s /Applications "$dmgsrc/Applications"
for attempt in 1 2 3; do
  if hdiutil create -volname "Postman $version" -srcfolder "$dmgsrc" -format UDZO -ov "$dmg" > /dev/null; then break; fi
  if [ "$attempt" = 3 ]; then echo "hdiutil create failed" >&2; exit 1; fi
  sleep 5
done
rm -rf "$(dirname "$dmgsrc")"
hdiutil verify "$dmg" > /dev/null
if [ -n "$identity" ]; then
  codesign --force --timestamp --sign "$identity" "$dmg"
fi
if [ -n "$notarize" ]; then
  notarize_file "$dmg"
  xcrun stapler staple "$dmg"
fi

if [ -n "${GITHUB_OUTPUT:-}" ]; then
  {
    echo "name=$name"
    echo "app=$app"
    echo "zip=$zip"
    echo "dmg=$dmg"
    echo "dir=$assets"
    echo "assets=$assets"
    echo "version=$version"
  } >> "$GITHUB_OUTPUT"
fi
echo "package: $zip"
echo "package: $dmg"
