#!/bin/bash
# The one human-assisted test: the real microphone path. A shell-launched binary cannot get the Microphone
# (the terminal has no grant), so this builds a debug .app with its own identity (app.susurro.desktop.e2e),
# launches it with `open`, waits for you to grant Microphone + Accessibility, then plays a fixture through the
# speakers with afplay while it dictates into a TextEdit doc it opens, and checks the pasted text.
#   scripts/e2e/real-mic.sh [fixture.wav]   (default en_custom.wav; turn the speakers up, mic near them)
#   scripts/e2e/real-mic.sh --check         build + launch + read permissions, then stop (no prompts, no audio)
#   scripts/e2e/real-mic.sh --reset         forget the grants of the test app (tccutil reset)
# Env: E2E_SKIP_BUILD=1, E2E_WAIT_S=300 (grant timeout). Exit 0 = pass.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
id=app.susurro.desktop.e2e
if [ "${1:-}" = "--reset" ]; then
  for s in Microphone Accessibility; do tccutil reset "$s" "$id"; done; exit 0
fi
mode=run; [ "${1:-}" = "--check" ] && { mode=check; shift; }
fixture="${1:-$here/fixtures/en_custom.wav}"
dir=/tmp/susurro-e2e-realmic port=47960 c="http://127.0.0.1:47960"
E="$here/lib/e2e"; doc=/tmp/susurro-e2e-realmic.txt
target="$root/src-tauri/target/e2e-app"
app="$target/debug/bundle/macos/susurro-e2e.app"

if pgrep -x TextEdit >/dev/null && [ $mode = run ]; then
  echo "Quit TextEdit first: this test opens its own doc and must not paste into yours." >&2; exit 1
fi
[ -f "$fixture" ] || "$here/fixtures/make.sh" >/dev/null

if [ -z "${E2E_SKIP_BUILD:-}" ]; then
  echo "building $app (debug, test hooks on)..."
  (cd "$root" && CARGO_TARGET_DIR="$target" bunx tauri build --debug --bundles app --no-sign \
    --config '{"identifier":"'$id'","productName":"susurro-e2e"}' >"$dir.build.log" 2>&1) \
    || { echo "build failed, see $dir.build.log" >&2; exit 1; }
fi
[ -d "$app" ] || { echo "missing $app" >&2; exit 1; }

rm -rf "$dir"; mkdir -p "$dir/models"
for m in "$HOME"/.cache/susurro/*.bin; do ln -sf "$m" "$dir/models/"; done
cat >"$dir/settings.json" <<'JSON'
{"onboarded": true, "language": "en",
 "hotkeys": {"ptt": "Ctrl+Alt+F20", "hands_free": "Ctrl+Shift+F20", "double_tap": true, "cancel": "Alt+Shift+F20", "paste_last": "Ctrl+Cmd+F20"},
 "stt": {"provider": "whisper_local", "whisper_model": "ggml-base.en-q5_1.bin"},
 "cleanup": {"level": "none"}, "recording": {"sounds": false}}
JSON

cleanup() {
  [ -f "$doc" ] && { "$here/lib/textedit.sh" close "$doc"; rm -f "$doc"; }
  pkill -f "$app/Contents/MacOS/" 2>/dev/null; sleep 0.5; pkill -9 -f "$app/Contents/MacOS/" 2>/dev/null
  rm -rf "$dir" "$dir.build.log"
}
trap cleanup EXIT

open -n "$app" --env SUSURRO_DATA_DIR="$dir" --env SUSURRO_TEST_CONTROL=$port \
  --env SUSURRO_TEST_PASTE_TARGET=com.apple.TextEdit --stdout "$dir/app.log" --stderr "$dir/app.log"
for _ in $(seq 1 100); do curl -sf "$c/state" >/dev/null && break; sleep 0.3; done
curl -sf "$c/state" >/dev/null || { echo "app did not answer on $c" >&2; exit 1; }
perm() { curl -sf "$c/permissions"; }
echo "permissions: $(perm)"
[ $mode = check ] && { echo "check ok: the .app launches isolated and answers"; exit 0; }

# 1. Grants. The prompts belong to "susurro-e2e", not to your real susurro.
curl -sf -XPOST "$c/permissions/request/microphone" >/dev/null
curl -sf -XPOST "$c/permissions/request/accessibility" >/dev/null
echo ">> Allow Microphone for susurro-e2e, and turn on susurro-e2e in Settings > Privacy > Accessibility."
end=$((SECONDS + ${E2E_WAIT_S:-300}))
until p=$(perm) && [ "$(jq -r .microphone <<<"$p")" = granted ] && [ "$(jq -r .accessibility <<<"$p")" = true ]; do
  [ $SECONDS -gt $end ] && { echo "FAIL: grants missing after ${E2E_WAIT_S:-300} s: $p" >&2; exit 1; }
  sleep 1
done
echo "grants ok"
curl -sf -XPOST "$c/dict" -d @"$here/fixtures/dict.json" >/dev/null
sleep 2 # whisper context loads in the background

# 2. Dictate: TextEdit doc in front, hold, play the fixture through the speakers, release.
pid=$("$here/lib/textedit.sh" open "$doc"); sleep 1.5
[ "$("$E" front | jq -r .bundle_id)" = com.apple.TextEdit ] || { echo "FAIL: TextEdit is not frontmost" >&2; exit 1; }
before=$(curl -sf "$c/state" | jq '.last_history.id // 0')
curl -sf -XPOST "$c/event/hold_start" >/dev/null; sleep 0.4
afplay "$fixture"; sleep 0.6
curl -sf -XPOST "$c/event/hold_end" >/dev/null
for _ in $(seq 1 120); do
  s=$(curl -sf "$c/state"); [ "$(jq '.last_history.id // 0' <<<"$s")" != "$before" ] && break; sleep 0.25
done
sleep 0.8
row=$(jq -c '.last_history | {status, provider, latency_ms, raw, final_text, error}' <<<"$s")
pasted=$("$E" axvalue "$pid" "$(basename "$doc")" 2>&1)
echo "row:    $row"
echo "pasted: $pasted"
grep -E "mic|device|record" "$dir/logs/susurro.log" | tail -3

# 3. Checks: pasted, and most of the fixture's words came through the air.
fails=0
[ "$(jq -r .status <<<"$row")" = pasted ] || { echo "FAIL: status is not pasted"; fails=1; }
want=$(jq -r --arg f "$(basename "$fixture")" '.files[] | select(.file == $f) | .text' "$here/fixtures/expect.json")
score=$(python3 - "$want" "$pasted" <<'PY'
import re, sys
w = lambda s: re.findall(r"\w+", s.lower())
ref, hyp = w(sys.argv[1]), set(w(sys.argv[2]))
print(f"{sum(t in hyp for t in ref) / max(len(ref), 1):.2f}")
PY
)
echo "word recall: $score (want >= 0.70)"
python3 -c "import sys; sys.exit(0 if float('$score') >= 0.70 else 1)" || { echo "FAIL: too few words came through"; fails=1; }
[ $fails = 0 ] && echo "PASS real mic path" && echo "Undo the grants later: scripts/e2e/real-mic.sh --reset"
exit $fails
