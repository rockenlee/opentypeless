#!/usr/bin/env bash
# Canonical build + install for OpenTypeless on macOS.
#
# WHY THIS EXISTS:
#   1. "Loading the old package" bugs came from copying bundle dirs with `cp -r`,
#      which doesn't reliably overwrite the installed binary. This script does a
#      clean `rm -rf` + fresh copy so there is exactly one source of truth.
#   2. "Middle-click stops working after every rebuild" came from ad-hoc signing
#      ("-"). macOS TCC keys the Accessibility grant on the binary's cdhash for
#      ad-hoc apps, and the cdhash changes every build → grant silently dropped.
#      tauri.conf.json now signs with a stable Apple Development identity, so the
#      grant is keyed on (certificate + bundle id) and survives rebuilds. This
#      script verifies the output is NOT ad-hoc and fails loudly if it is.
#
# Usage:  ./scripts/build-install.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$ROOT"
export PATH="$HOME/.cargo/bin:$PATH"

APP_NAME="OpenTypeless"
BUNDLE_ID="com.opentypeless.app"
BUILT_APP="$ROOT/src-tauri/target/release/bundle/macos/$APP_NAME.app"
INSTALLED_APP="/Applications/$APP_NAME.app"
BIN_REL="Contents/MacOS/opentypeless"

echo "==> Building (npm run tauri build)"
npm run tauri build

if [ ! -d "$BUILT_APP" ]; then
  echo "ERROR: expected $BUILT_APP after build, not found" >&2
  exit 1
fi

# --- Guard: the built bundle must NOT be ad-hoc signed. ---
SIG_LINE="$(codesign -dv --verbose=4 "$BUILT_APP" 2>&1 | grep -E '^Signature' || true)"
if echo "$SIG_LINE" | grep -qi adhoc; then
  echo "ERROR: built app is ad-hoc signed ($SIG_LINE)." >&2
  echo "       Set bundle.macOS.signingIdentity in tauri.conf.json to a stable" >&2
  echo "       Apple Development / Developer ID identity, or middle-click will" >&2
  echo "       break on every rebuild (TCC drops the Accessibility grant)." >&2
  exit 1
fi
echo "==> Built app signature OK (not ad-hoc): $SIG_LINE"

# --- Clean install: stop, remove old, copy fresh. ---
echo "==> Stopping running app"
pkill -x opentypeless 2>/dev/null || true
sleep 1

echo "==> Removing old install at $INSTALLED_APP"
rm -rf "$INSTALLED_APP"

echo "==> Installing fresh bundle"
cp -R "$BUILT_APP" "$INSTALLED_APP"

# --- Verify the installed binary matches the freshly built one. ---
BUILT_HASH="$(shasum -a 256 "$BUILT_APP/$BIN_REL" | awk '{print $1}')"
INST_HASH="$(shasum -a 256 "$INSTALLED_APP/$BIN_REL" | awk '{print $1}')"
if [ "$BUILT_HASH" != "$INST_HASH" ]; then
  echo "ERROR: installed binary hash != built binary hash — stale copy!" >&2
  exit 1
fi
echo "==> Installed binary verified (sha256 ${INST_HASH:0:12}…)"

# --- Sync DMG to Downloads (single canonical location). ---
DMG="$(ls -t "$ROOT"/src-tauri/target/release/bundle/dmg/*.dmg 2>/dev/null | head -1 || true)"
if [ -n "$DMG" ]; then
  cp -f "$DMG" "$HOME/Downloads/"
  echo "==> DMG copied to ~/Downloads/$(basename "$DMG")"
fi

echo ""
echo "✅ Done. Installed: $INSTALLED_APP"
echo ""
echo "First run after switching signing identity (ad-hoc → Apple Development):"
echo "  macOS sees a NEW code identity, so you must re-grant Accessibility ONCE:"
echo "    System Settings → Privacy & Security → Accessibility"
echo "    → remove OpenTypeless if present, then re-add /Applications/$APP_NAME.app"
echo "  After this one-time re-grant, future rebuilds keep the permission."
echo ""
echo "  (Optional hard reset if it still misbehaves:)"
echo "    tccutil reset Accessibility $BUNDLE_ID"
