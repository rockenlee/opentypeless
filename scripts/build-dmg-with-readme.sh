#!/usr/bin/env bash
# Build a DISTRIBUTABLE macOS DMG for sharing with other people.
#
# Output: a compressed DMG containing OpenTypeless.app + an /Applications
# symlink + the bilingual install guide, copied to ~/Downloads.
#
#   ./scripts/build-dmg-with-readme.sh
#
# Signing: this script ad-hoc signs the app ("-"), regardless of what
# tauri.conf.json pins. Rationale:
#   * tauri.conf.json normally pins an "Apple Development" cert, which is right
#     for the DEVELOPER's own machine (stable Accessibility/TCC grant) but will
#     NOT run on anyone else's Mac (Gatekeeper rejects Development certs).
#   * Ad-hoc signing runs on any Mac once the recipient clears quarantine
#     (see INSTALL_macOS.md), and Tauri still embeds Entitlements.plist on an
#     ad-hoc sign (mic + automation entitlements land — unsigned builds lose
#     them).
# We flip the config to "-" only for this build and ALWAYS restore it on exit
# (even on failure), so day-to-day dev builds keep their Development cert.
#
# NOTE: this does NOT touch /Applications/OpenTypeless.app — your own installed
# copy (Development-signed, stable TCC) is left alone.
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
CONF="$ROOT/src-tauri/tauri.conf.json"

# --- Restore-on-exit guard (config + temp stage) -------------------------------
CONF_BAK="$(mktemp -t tauri-conf-bak)"
cp "$CONF" "$CONF_BAK"
STAGE=""
cleanup() {
  # Restore the original tauri.conf.json (un-flip signingIdentity).
  if [ -f "$CONF_BAK" ]; then
    mv -f "$CONF_BAK" "$CONF"
    echo "==> restored tauri.conf.json (signingIdentity un-flipped)"
  fi
  [ -n "$STAGE" ] && rm -rf "$STAGE" 2>/dev/null || true
}
trap cleanup EXIT

# --- Flip to ad-hoc signing for this build only --------------------------------
sed -i '' 's/"signingIdentity": *"[^"]*"/"signingIdentity": "-"/' "$CONF"
echo "==> distribution build: signingIdentity temporarily set to ad-hoc (\"-\")"

# 1. Build .app via Tauri (skip its DMG step — we bake our own with the README).
echo "==> tauri build (.app only, ad-hoc signed)"
npx tauri build --bundles app

if [ ! -d "$APP_BUNDLE" ]; then
  echo "ERROR: expected $APP_BUNDLE after build, not found" >&2
  exit 1
fi
if [ ! -f "$README_SRC" ]; then
  echo "ERROR: $README_SRC not found" >&2
  exit 1
fi

# Sanity-check: the bundle must be ad-hoc signed (Signature=adhoc), otherwise it
# won't run on other Macs. Abort if a real cert leaked through.
SIG="$(codesign -dvv "$APP_BUNDLE" 2>&1 || true)"
if echo "$SIG" | grep -q "Signature=adhoc"; then
  echo "==> verified: app is ad-hoc signed (distributable)"
else
  echo "ERROR: app is NOT ad-hoc signed — refusing to ship. codesign said:" >&2
  echo "$SIG" | grep -iE "Authority|Signature" >&2
  exit 1
fi

# 2. Stage a temp DMG source folder.
STAGE="$(mktemp -d -t opentypeless-dmg)"
echo "==> staging DMG contents in $STAGE"
ditto "$APP_BUNDLE" "$STAGE/OpenTypeless.app"
ln -s /Applications "$STAGE/Applications"
cp "$README_SRC" "$STAGE/请先阅读 — README.md"

# 3. Make the DMG. UDZO = compressed read-only DMG (the standard format).
VERSION="$(grep -m1 '"version":' "$CONF" | sed -E 's/.*"version": *"([^"]+)".*/\1/')"
DMG_NAME="OpenTypeless_${VERSION}_arm64.dmg"
DMG_PATH="$DMG_OUT_DIR/$DMG_NAME"
mkdir -p "$DMG_OUT_DIR"
rm -f "$DMG_PATH"

echo "==> hdiutil create $DMG_PATH"
hdiutil create -volname "OpenTypeless ${VERSION}" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$DMG_PATH" >/dev/null

# 4. Copy to ~/Downloads for easy sharing.
DEST="$HOME/Downloads/$DMG_NAME"
cp "$DMG_PATH" "$DEST"

echo ""
echo "==> DONE — distributable DMG ready"
echo "  $DEST"
ls -lah "$DEST"
echo ""
echo "Send this DMG to others. They must clear quarantine on first launch —"
echo "the steps are in '请先阅读 — README.md' inside the DMG."
