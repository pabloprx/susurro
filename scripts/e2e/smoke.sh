#!/bin/bash
# Proves the harness: launch, local whisper (ggml-base.bin), hold_start/hold_end over a fixture, poll /state, stop.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export E2E_DIR="${E2E_DIR:-/tmp/susurro-e2e-smoke}" E2E_PORT="${E2E_PORT:-47914}"
fixture="${1:-$here/fixtures/en_custom.wav}"
c="http://127.0.0.1:$E2E_PORT"
E2E_AUDIO="$fixture" "$here/run-app.sh" >/dev/null
trap '"$here/stop-app.sh" --clean >/dev/null' EXIT
curl -sf -X POST "$c/settings" -d '{"stt":{"provider":"whisper_local","whisper_model":"ggml-base.bin"},"language":"en"}' >/dev/null
curl -sf -X POST "$c/dict" -d @"$here/fixtures/dict.json" >/dev/null
sleep 3 # whisper context loads in the background
secs=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$fixture")
curl -sf -X POST "$c/event/hold_start" >/dev/null
sleep "$(echo "$secs + 0.5" | bc)"
curl -sf -X POST "$c/event/hold_end" >/dev/null
for _ in $(seq 1 120); do
  item=$(curl -sf "$c/state" | jq -c '.last_history // empty')
  [ -n "$item" ] && break
  sleep 0.5
done
[ -n "$item" ] || { echo "FAIL: no history item"; tail -30 "$E2E_DIR/app.log"; exit 1; }
echo "$item" | jq '{status, provider, duration_ms, latency_ms, raw, final_text}'
grep -E "test audio|test paste guard" "$E2E_DIR/app.log" | sed 's/^.*\] //' || true
