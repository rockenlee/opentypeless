#!/usr/bin/env bash
# Rebuild the macOS DMG with INSTALL_macOS.md included alongside the .app.
# Tauri's default DMG contains only OpenTypeless.app + an /Applications symlink;
# we want a README right next to the .app so the install steps + permission
# instructions are unmissable.
#
# Usage:  ./scripts/build-dmg-with-readme.sh
# Output: src-tauri/target/release/bundle/dmg/OpenTypeless_<ver>_arm64.dmg
#         (also copied to ~/Desktop)
set -euo pipefail

# Resolve project root (this script lives in <root>/scripts/).
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"

# Prefer the rustup-managed arm64 toolchain over any brew-installed x86_64 rustc.
export PATH="$HOME/.cargo/bin:$PATH"

APP_BUNDLE="$ROOT/src-tauri/target/release/bundle/macos/OpenTypeless.app"
README_SRC="$ROOT/INSTALL_macOS.md"
DMG_OUT_DIR="$ROOT/src-tauri/target/release/bundle/dmg"

# 1. Build .app via Tauri (skip its DMG step — we'll bake our own).
# `tauri build --bundles app` produces only the .app, no DMG.
echo "==> tauri build (.app only)"
npx tauri build --bundles app

if [ ! -d "$APP_BUNDLE" ]; then
  echo "ERROR: expected $APP_BUNDLE after build, not found" >&2
  exit 1
fi
if [ ! -f "$README_SRC" ]; then
  echo "ERROR: $README_SRC not found" >&2
  exit 1
fi

# 2. Stage a temp DMG source folder.
STAGE="$(mktemp -d -t opentypeless-dmg)"
trap 'rm -rf "$STAGE" 2>/dev/null || true' EXIT

echo "==> staging DMG contents in $STAGE"
ditto "$APP_BUNDLE" "$STAGE/OpenTypeless.app"
ln -s /Applications "$STAGE/Applications"
cp "$README_SRC" "$STAGE/请先阅读 — README.md"

# 3. Make the DMG. UDZO = compressed read-only DMG (the standard format).
VERSION="$(grep -m1 '"version":' "$ROOT/src-tauri/tauri.conf.json" | sed -E 's/.*"version": *"([^"]+)".*/\1/')"
DMG_NAME="OpenTypeless_${VERSION}_arm64.dmg"
DMG_PATH="$DMG_OUT_DIR/$DMG_NAME"
mkdir -p "$DMG_OUT_DIR"
rm -f "$DMG_PATH"
rm -f "$DMG_OUT_DIR/OpenTypeless_${VERSION}_x64.dmg"  # purge stale generic name

echo "==> hdiutil create $DMG_PATH"
hdiutil create -volname "OpenTypeless ${VERSION}" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$DMG_PATH" >/dev/null

# 4. Copy to Desktop for convenience.
DESK="$HOME/Desktop/$DMG_NAME"
cp "$DMG_PATH" "$DESK"

echo ""
echo "==> DONE"
echo "  bundle: $DMG_PATH"
echo "  desktop: $DESK"
ls -la "$DESK"
echo ""
echo "==> DMG contents preview:"
hdiutil attach "$DMG_PATH" -nobrowse -readonly 2>&1 | grep "/Volumes" | head -1
MOUNT="/Volumes/OpenTypeless ${VERSION}"
ls -la "$MOUNT" 2>&1
hdiutil detach "$MOUNT" 2>&1 | head -1
