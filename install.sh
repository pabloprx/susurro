#!/usr/bin/env bash
# One command: build the release app, check its signature, replace /Applications/susurro.app.
#   ./install.sh            build + install + open susurro (CLAUDE.local.md: one command, relaunch)
#   ./install.sh --no-open  build + install, do not open it
#   ./install.sh --check    build + signature check only (installs nothing)
# The signing identity lives in src-tauri/tauri.conf.json (bundle.macOS.signingIdentity). A stable
# identity keeps the macOS permission grants (Accessibility, Input Monitoring) across rebuilds.
set -euo pipefail

BUNDLE_ID="app.susurro.desktop"
DEST="/Applications/susurro.app"
OPEN=1
CHECK=0
for a in "$@"; do
  case "$a" in
    --open) OPEN=1 ;;
    --no-open) OPEN=0 ;;
    --check) CHECK=1 ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
APP="src-tauri/target/release/bundle/macos/susurro.app"

[ -d node_modules ] || bun install
echo "==> Building susurro (release)"
bun tauri build --bundles app

echo "==> Checking the signature"
codesign --verify --deep --strict "$APP"
info="$(codesign -dv --verbose=4 "$APP" 2>&1)"
req="$(codesign -d -r- "$APP" 2>&1)"
grep -q "^Identifier=$BUNDLE_ID$" <<<"$info" || { echo "FAIL: identifier is not $BUNDLE_ID" >&2; echo "$info" >&2; exit 1; }
grep -q "^Authority=Apple Development" <<<"$info" || { echo "FAIL: not signed with an Apple Development identity" >&2; echo "$info" >&2; exit 1; }
if ! grep -q 'certificate leaf' <<<"$req" || grep -q 'cdhash' <<<"$req"; then
  echo "FAIL: the designated requirement is not identity-based, macOS would forget the grants" >&2
  echo "$req" >&2
  exit 1
fi
echo "    $(grep '^Authority=' <<<"$info" | head -1)"
echo "    $(grep '^TeamIdentifier=' <<<"$info")"

if [ "$CHECK" = 1 ]; then
  echo "OK: $APP is signed and ready (--check: nothing installed)"
  exit 0
fi

# Grants made for an old ad hoc build (Microphone too) stay ON in System Settings but never match the new signature:
# reset them once, so macOS asks one last time and remembers.
RESET=0
if [ -d "$DEST" ] && codesign -dv "$DEST" 2>&1 | grep -q "Signature=adhoc"; then
  RESET=1
fi

# Only the installed app: `tauri dev` (target/debug/susurro) keeps running. A plain SIGTERM quits
# susurro cleanly (no osascript: that asks for Automation consent and can quit the new app later).
RUNNING="$DEST/Contents/MacOS/susurro"
if pgrep -f "^$RUNNING" >/dev/null; then
  echo "==> Quitting the running susurro"
  pkill -f "^$RUNNING" || true
  for _ in $(seq 1 50); do
    pgrep -f "^$RUNNING" >/dev/null || break
    sleep 0.1
  done
  pkill -9 -f "^$RUNNING" 2>/dev/null || true
fi

echo "==> Installing to $DEST"
rm -rf "$DEST"
ditto "$APP" "$DEST"
codesign --verify --deep --strict "$DEST"

if [ "$RESET" = 1 ]; then
  echo "==> The old build was signed ad hoc: resetting its Accessibility, Input Monitoring and Microphone entries"
  tccutil reset Accessibility "$BUNDLE_ID" || true
  tccutil reset ListenEvent "$BUNDLE_ID" || true
  tccutil reset Microphone "$BUNDLE_ID" || true
fi

echo
echo "Installed $(defaults read "$DEST/Contents/Info" CFBundleShortVersionString 2>/dev/null || echo susurro)."
if [ "$RESET" = 1 ]; then
  echo "macOS asks for the permissions one more time. After this, rebuilds keep the grants."
fi
if [ "$OPEN" = 1 ]; then
  open "$DEST"
  echo "susurro is open. Hold your talk key (F1 by default) and speak."
else
  echo "Open it: open -a susurro"
fi
