#!/bin/bash
# One push-to-talk dictation of a fixture through the running test app; prints the new history row (or the
# session message when no row is written: silence, too short, "Heard nothing").
#   E2E_PORT=47913 lib/dictate.sh <fixture.wav> [timeout_s=90]
set -euo pipefail
c="http://127.0.0.1:${E2E_PORT:-47913}"
f="$1"; to="${2:-90}"
case "$f" in /*) ;; *) f="$(cd "$(dirname "$0")/../fixtures" && pwd)/$f" ;; esac
before=$(curl -sf "$c/state" | jq '.last_history.id // 0')
curl -sf -X POST "$c/audio" --data-binary "$f" >/dev/null
secs=$(ffprobe -v error -show_entries format=duration -of csv=p=0 "$f")
curl -sf -X POST "$c/event/hold_start" >/dev/null
sleep "$(echo "$secs + 0.4" | bc)"
curl -sf -X POST "$c/event/hold_end" >/dev/null
msg=""
for _ in $(seq 1 $((to * 4))); do
  s=$(curl -sf "$c/state")
  id=$(echo "$s" | jq '.last_history.id // 0')
  [ "$id" != "$before" ] && { echo "$s" | jq -c '.last_history | {status, provider, cleanup, latency_ms, raw, final_text, error}'; exit 0; }
  m=$(echo "$s" | jq -r '.session.message // empty'); [ -n "$m" ] && msg="$m"
  [ "$(echo "$s" | jq -r .session.phase)" = idle ] && [ -n "$msg" ] && { jq -cn --arg m "$msg" '{status: "no_row", message: $m}'; exit 0; }
  sleep 0.25
done
jq -cn --arg m "$msg" '{status: "timeout", message: $m}'
