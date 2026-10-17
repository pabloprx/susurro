#!/bin/bash
# Device management against real CoreAudio: aggregate devices are plugged/unplugged while the real app runs.
# Needs no Microphone permission (selection logic only; real-cpal clips are zeros and hit the silence gate).
# Usage: scripts/e2e/devices-real.sh   (E2E_SKIP_BUILD=1 to reuse the build). Exit 1 if any check fails.
# Checks under "Regression" were app bugs fixed on 2026-10-17.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
export E2E_DIR=/tmp/susurro-e2e-devices E2E_PORT=47931
E="$here/lib/e2e"; P="http://127.0.0.1:$E2E_PORT"; L="$E2E_DIR/logs/susurro.log"; F="$here/fixtures"
made=(); fails=0; pass=0

cleanup() {
  "$here/stop-app.sh" --clean >/dev/null
  for u in "${made[@]}"; do "$E" agg destroy "$u" 2>/dev/null; done
}
trap cleanup EXIT

plug() { "$E" agg create "$1" "susurro-e2e-dev-$2" >/dev/null && made+=("susurro-e2e-dev-$2"); }
unplug() { "$E" agg destroy "susurro-e2e-dev-$1"; }
id() { echo "coreaudio:susurro-e2e-dev-$1"; }
tick() { sleep 2.5; } # > one 1.5 s monitor poll
q() { curl -s "$P/state" | python3 -c "import json,sys; s=json.load(sys.stdin); m=s['mic']; print($1)"; }
post() { curl -s -XPOST "$P$1" --data-binary "${2:-}" >/dev/null; }
mark() { wc -l <"$L"; }
toasts_since() { tail -n +"$(($1 + 1))" "$L" | grep -o 'toast: .*' | cut -c8-; }
check() { # name, got, want
  if [ "$2" = "$3" ]; then pass=$((pass + 1)); echo "ok   $1"; else fails=$((fails + 1)); echo "FAIL $1: got [$2] want [$3]"; fi
}
sel() { q "m['selection'] if m['selection']=='auto' else m['selection']['device']['id']"; }
active() { q "m['active']"; }
prio() { q "','.join(k['id'].split('dev-')[-1] for k in m['priority'] if 'susurro-e2e' in k['id'])"; }

"$here/run-app.sh" >/dev/null || exit 1
sleep 2
# The seeded model is English-only.
post /settings '{"language":"en"}'

# 1. Hidden virtual device: no prompt while show_virtual is off.
m=$(mark); plug "Hidden" hid; tick
check "virtual device hidden, no prompt" "$(toasts_since $m | grep -c 'New mic')" 0
unplug hid; tick

# 2. New device -> "New mic" prompt; Switch selects it and puts it on top of the priority list.
post /settings '{"mic":{"show_virtual":true}}'
m=$(mark); plug "E2E A" a; tick
check "new device prompt" "$(toasts_since $m)" "New mic: E2E A"
post /toast_action "{\"toast_id\":\"mic-new:$(id a)\",\"action_id\":\"switch\"}"; sleep 0.5
check "switch selects it" "$(sel)" "$(id a)"
check "switch puts it first" "$(prio)" "a"

# 3. Keep current: remembered at the end of the list, selection unchanged, no repeat prompt.
plug "E2E B" b; tick
post /toast_action "{\"toast_id\":\"mic-new:$(id b)\",\"action_id\":\"keep\"}"; sleep 0.5
check "keep appends" "$(prio)" "a,b"
check "keep leaves selection" "$(sel)" "$(id a)"
unplug b; tick; m=$(mark); plug "E2E B" b; tick
check "kept device: no repeat prompt" "$(toasts_since $m | grep -c 'New mic')" 0
post /mic/priority "[\"$(id a)\",\"$(id b)\"]" # known devices left out (the real mic) go after these

# 4. Selected device removed -> next priority device, fallback_from + toast; re-added -> restored + toast.
m=$(mark); unplug a; tick
check "fallback to next priority" "$(active)" "$(id b)"
check "fallback_from" "$(q "m['fallback_from']")" "E2E A"
check "fallback toast" "$(toasts_since $m)" "E2E A disconnected - using E2E B"
m=$(mark); plug "E2E A" a; tick
check "restored" "$(active)" "$(id a)"
check "restore toast" "$(toasts_since $m)" "Using E2E A again"

# 5. Same device, new id (name heal): selection and priority follow the new id, no prompt.
unplug a; tick; m=$(mark); plug "E2E A" a2; tick
check "healed id selected" "$(sel)" "$(id a2)"
check "heal: no prompt, restore toast" "$(toasts_since $m)" "Using E2E A again"

# 6. Two known same-name devices: removing the selected one falls back by priority, not to its twin.
plug "E2E A" twin; tick
post /toast_action "{\"toast_id\":\"mic-new:$(id twin)\",\"action_id\":\"keep\"}"; sleep 0.5
unplug a2; tick
check "known twin is not the selected device" "$(active)" "$(id b)"
plug "E2E A" a2; tick
check "known twin: original restored" "$(active)" "$(id a2)"
post /mic/forget "\"$(id twin)\""; unplug twin; tick

# 7. Regression (fixed 2026-10-17): an unknown same-name device steals the selection when the selected one is unplugged.
plug "E2E A" stranger; tick       # "New mic: E2E A" prompt, ignored
m=$(mark); unplug a2; tick
check "unknown twin must not take over the selection" "$(sel)" "$(id a2)"
check "unplug of selected device must toast" "$(toasts_since $m | grep -c disconnected)" 1
post /mic/select '"auto"'; post /mic/forget "\"$(id stranger)\""; post /mic/forget "\"$(id a2)\""; unplug stranger; tick
post /mic/select "{\"device\":{\"id\":\"$(id b)\"}}"; plug "E2E A" a2; tick
post /toast_action "{\"toast_id\":\"mic-new:$(id a2)\",\"action_id\":\"keep\"}"; sleep 0.5

# 8. Regression (fixed 2026-10-17): Keep current / Switch on a prompt whose device was unplugged in the meantime.
plug "E2E Late" late; tick; unplug late; tick
post /toast_action "{\"toast_id\":\"mic-new:$(id late)\",\"action_id\":\"keep\"}"; sleep 0.5
m=$(mark); plug "E2E Late" late; tick
check "keep after unplug must remember (no repeat prompt)" "$(toasts_since $m | grep -c 'New mic')" 0
unplug late; tick
post /toast_action "{\"toast_id\":\"mic-new:$(id late)\",\"action_id\":\"switch\"}"; sleep 0.5
check "switch after unplug: fallback_from is a name" "$(q "m['fallback_from']")" "E2E Late"
m=$(mark); plug "E2E Late" late; tick
check "switch after unplug: one clean restore toast" "$(toasts_since $m)" "Using E2E Late again"
post /mic/select "{\"device\":{\"id\":\"$(id b)\"}}"; post /mic/forget "\"$(id late)\""; unplug late; tick

# 9. Priority order + selection persist across a restart; a device plugged while the app was off is prompted.
post /mic/select '"auto"'
post /mic/priority "[\"$(id a2)\",\"$(id b)\"]"
want=$(prio)
"$here/stop-app.sh" >/dev/null
plug "E2E Offline" off; m=$(mark)
E2E_SKIP_BUILD=1 "$here/run-app.sh" >/dev/null || exit 1
tick
check "priority persisted" "$(prio)" "$want"
check "auto picks first priority" "$(active)" "$(id a2)"
check "plugged while off: prompt" "$(toasts_since $m | grep -c 'New mic: E2E Offline')" 1

# 10. Selected device missing at startup -> fallback toast at startup.
post /mic/select "{\"device\":{\"id\":\"$(id b)\"}}"
"$here/stop-app.sh" >/dev/null; unplug b; m=$(mark)
E2E_SKIP_BUILD=1 "$here/run-app.sh" >/dev/null || exit 1
tick
check "startup fallback toast" "$(toasts_since $m | grep -c 'E2E B disconnected - using E2E A')" 1
plug "E2E B" b; tick

# 11. Loss during a dictation (real cpal on the aggregate): continue on the next device, keep the audio.
post /audio ''
post /event/toggle; sleep 1
check "dictation starts on selected" "$(q "s['session']['device']")" "E2E B"
m=$(mark); unplug b; sleep 3
check "mic lost toast" "$(toasts_since $m | grep -c 'Mic lost - continuing on E2E A')" 1
check "session.device follows the switch" "$(q "s['session']['device']")" "E2E A"
post /event/stop; sleep 3; plug "E2E B" b; tick

# 12. Loss during a dictation with test audio: both segments end up in one transcript.
post /audio "$F/en_custom.wav:$F/en_numbers.wav"
post /event/toggle; sleep 3.5; unplug b; sleep 7; post /event/stop
for _ in $(seq 1 40); do [ "$(q "s['session']['phase']")" = idle ] && break; sleep 0.5; done
raw=$(q "(s['last_history'] or {}).get('raw','')")
check "transcript has segment 1" "$(echo "$raw" | grep -ci 'contaflow')" 1
check "transcript has segment 2" "$(echo "$raw" | grep -c '1,250')" 1
plug "E2E B" b; tick

# 13. Freshness at dictation start. Unplugged < 1.5 s ago: the start retries on a fresh list.
post /audio ''
unplug b; post /event/toggle; sleep 0.8
check "just-unplugged: falls back" "$(q "s['session']['device']")" "E2E A"
post /event/stop; sleep 3
# Regression (fixed 2026-10-17): re-plugged < 1.5 s ago: the start uses the monitor's stale list and ignores the selected device.
plug "E2E B" b; post /event/toggle; sleep 0.8
check "just-plugged selected device is used" "$(q "s['session']['device']")" "E2E B"
post /event/stop; sleep 3

echo "pass $pass, fail $fails"
[ "$fails" = 0 ]
