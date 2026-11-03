#!/usr/bin/env bash
# One command: build the release app, check its signature, replace /Applications/susurro.app.
#   ./install.sh            build + install + open susurro (one command, relaunch)
#   ./install.sh --no-open  build + install, do not open it
#   ./install.sh --check    build + signature check only (installs nothing)
#   ./install.sh --fresh    uninstall first: delete settings, history, keys, models and permission grants,
#                           then install and open it like a first run
# Signing: SUSURRO_SIGNING_IDENTITY if set, else the identity in src-tauri/tauri.conf.json
# (bundle.macOS.signingIdentity) if this Mac has it, else this Mac's first Apple Development identity,
# else ad hoc. A stable identity keeps the macOS permission grants (Accessibility, Input Monitoring)
# across rebuilds; an ad hoc build works, but macOS asks for the permissions again after every rebuild.
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
    -h|--help) sed -n '2,11p' "$0"; exit 0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
APP="src-tauri/target/release/bundle/macos/susurro.app"

for tool in bun cargo cmake; do
  command -v "$tool" >/dev/null || { echo "Missing $tool. See README.md, Requirements." >&2; exit 1; }
done

IDENTITY="${SUSURRO_SIGNING_IDENTITY:-}"
if [ -z "$IDENTITY" ]; then
  ids="$(security find-identity -v -p codesigning 2>/dev/null || true)"
  conf="$(sed -n 's/.*"signingIdentity": *"\([^"]*\)".*/\1/p' src-tauri/tauri.conf.json)"
  if [ -n "$conf" ] && grep -q "$conf" <<<"$ids"; then
    IDENTITY="$conf"
  else
    IDENTITY="$(grep 'Apple Development' <<<"$ids" | head -1 | awk '{print $2}')"
  fi
fi
ADHOC=0
if [ -z "$IDENTITY" ] || [ "$IDENTITY" = "-" ]; then
  IDENTITY="-"
  ADHOC=1
  echo "==> No Apple Development identity found: signing ad hoc (macOS asks for permissions again after each rebuild)"
fi
# The Tauri CLI prefers this over bundle.macOS.signingIdentity.
export APPLE_SIGNING_IDENTITY="$IDENTITY"

[ -d node_modules ] || bun install
echo "==> Building susurro (release)"
bun tauri build --bundles app

echo "==> Checking the signature"
codesign --verify --deep --strict "$APP"
info="$(codesign -dv --verbose=4 "$APP" 2>&1)"
req="$(codesign -d -r- "$APP" 2>&1)"
grep -q "^Identifier=$BUNDLE_ID$" <<<"$info" || { echo "FAIL: identifier is not $BUNDLE_ID" >&2; echo "$info" >&2; exit 1; }
if [ "$ADHOC" = 1 ]; then
  echo "    ad hoc signature"
else
  grep -q "^Authority=Apple Development" <<<"$info" || { echo "FAIL: not signed with an Apple Development identity" >&2; echo "$info" >&2; exit 1; }
  if ! grep -q 'certificate leaf' <<<"$req" || grep -q 'cdhash' <<<"$req"; then
    echo "FAIL: the designated requirement is not identity-based, macOS would forget the grants" >&2
    echo "$req" >&2
    exit 1
  fi
  echo "    $(grep '^Authority=' <<<"$info" | head -1)"
  echo "    $(grep '^TeamIdentifier=' <<<"$info")"
fi

if [ "$CHECK" = 1 ]; then
  echo "OK: $APP is signed and ready (--check: nothing installed)"
  exit 0
fi

# Grants made for an old ad hoc build (Microphone too) stay ON in System Settings but never match the new signature:
# reset them, so macOS asks again (once, with a stable identity; after every rebuild, ad hoc).
RESET=0
if [ "$ADHOC" = 1 ] || { [ -d "$DEST" ] && codesign -dv "$DEST" 2>&1 | grep -q "Signature=adhoc"; }; then
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
  echo "==> Resetting the Accessibility, Input Monitoring, Microphone and Screen Recording entries (ad hoc signature)"
  tccutil reset Accessibility "$BUNDLE_ID" || true
  tccutil reset ListenEvent "$BUNDLE_ID" || true
  tccutil reset Microphone "$BUNDLE_ID" || true
  tccutil reset ScreenCapture "$BUNDLE_ID" || true
fi

echo
echo "Installed $(defaults read "$DEST/Contents/Info" CFBundleShortVersionString 2>/dev/null || echo susurro)."
if [ "$ADHOC" = 1 ]; then
  echo "macOS asks for the permissions again (ad hoc builds lose them on every rebuild; see README.md, Signing)."
elif [ "$RESET" = 1 ]; then
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
