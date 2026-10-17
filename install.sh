#!/usr/bin/env bash
# One command: build the release app, check its signature, replace /Applications/susurro.app.
#   ./install.sh            build + install + open susurro (CLAUDE.local.md: one command, relaunch)
#   ./install.sh --no-open  build + install, do not open it
#   ./install.sh --check    build + signature check only (installs nothing)
#   ./install.sh --fresh    uninstall first: delete settings, history, keys, models and permission grants,
#                           then install and open it like a first run
# The signing identity lives in src-tauri/tauri.conf.json (bundle.macOS.signingIdentity). A stable
# identity keeps the macOS permission grants (Accessibility, Input Monitoring) across rebuilds.
set -euo pipefail

BUNDLE_ID="app.susurro.desktop"
DEST="/Applications/susurro.app"
OPEN=1
CHECK=0
FRESH=0
for a in "$@"; do
  case "$a" in
    --open) OPEN=1 ;;
    --no-open) OPEN=0 ;;
    --check) CHECK=1 ;;
    --fresh) FRESH=1 ;;
    -h|--help) sed -n '2,9p' "$0"; exit 0 ;;
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

if [ "$FRESH" = 1 ]; then
  echo "==> Fresh install: removing susurro data, keys and permission grants"
  rm -rf "$HOME/Library/Application Support/$BUNDLE_ID" "$HOME/Library/Logs/$BUNDLE_ID" \
    "$HOME/Library/Caches/$BUNDLE_ID" "$HOME/Library/WebKit/$BUNDLE_ID" "$HOME/Library/WebKit/susurro"
  # Old builds kept keys in the Keychain; remove those items too.
  for acct in openrouter elevenlabs local_stt local_llm; do
    security delete-generic-password -s "$BUNDLE_ID" -a "$acct" >/dev/null 2>&1 || true
  done
  launchctl remove "$BUNDLE_ID" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/susurro.plist"
  RESET=1
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
  tccutil reset ScreenCapture "$BUNDLE_ID" || true
fi

echo
echo "Installed $(defaults read "$DEST/Contents/Info" CFBundleShortVersionString 2>/dev/null || echo susurro)."
if [ "$RESET" = 1 ]; then
  echo "macOS asks for the permissions one more time. After this, rebuilds keep the grants."
fi
if [ "$OPEN" = 1 ]; then
  open "$DEST"
  # The user's own talk key from settings.json; F1 (the default) before the first run writes it.
  PTT="$(plutil -extract hotkeys.ptt raw -o - "$HOME/Library/Application Support/$BUNDLE_ID/settings.json" 2>/dev/null || true)"
  echo "susurro is open. Hold ${PTT:-F1} and speak."
else
  echo "Open it: open -a susurro"
fi
