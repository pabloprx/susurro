#!/bin/bash
# Full-app E2E scenarios 1-7 on the real debug app (run-app.sh), fixture audio instead of the mic.
#   scripts/e2e/scenarios.sh [1-7 ...]    (default: all; E2E_SKIP_BUILD=1 reuses the build). Exit 1 if any check fails.
# 1 custom words (event PTT)   2 real PTT hotkey, Spanish (large-v3-turbo)   3 hands-free, double tap, cancel
# 4 text rules (snippet, spoken punctuation, backtrack)   5 gates (silence, too short)   6 long dictation (46 s)
# 7 real paste into a TextEdit doc this script opens + paste last + clipboard restore. Skipped if TextEdit already runs.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
export E2E_DIR=/tmp/susurro-e2e-scenarios E2E_PORT=47941
E="$here/lib/e2e"; c="http://127.0.0.1:$E2E_PORT"; F="$here/fixtures"
pass=0; fails=0
doc=/tmp/susurro-e2e-paste.txt
cleanup() {
  [ -f "$doc" ] && { "$here/lib/textedit.sh" close "$doc"; rm -f "$doc"; }
  "$here/stop-app.sh" --clean >/dev/null
}
trap cleanup EXIT

check() { # name, got, want-substring (or "!x" = must not contain x)
  local ok=0
  case "$3" in "!"*) [[ "$2" != *"${3:1}"* ]] && ok=1 ;; *) [[ "$2" == *"$3"* ]] && ok=1 ;; esac
  if [ $ok = 1 ]; then pass=$((pass + 1)); echo "ok   $1"; else fails=$((fails + 1)); echo "FAIL $1: got [$2] want [$3]"; fi
}
st() { curl -sf "$c/state"; }
field() { printf '%s' "$1" | jq -r "$2"; }
dictate() { "$here/lib/dictate.sh" "$1" "${2:-90}"; }
set_() { curl -sf -XPOST "$c/settings" -d "$1" >/dev/null; sleep "${2:-0}"; }
wait_row() { # previous id -> new row JSON
  for _ in $(seq 1 240); do s=$(st); [ "$(field "$s" '.last_history.id // 0')" != "$1" ] && { field "$s" '.last_history|tojson'; return; }; sleep 0.25; done
  echo '{}'
}
last_id() { field "$(st)" '.last_history.id // 0'; }
phase() { field "$(st)" .session.phase; }
audio() { curl -sf -XPOST "$c/audio" --data-binary "$F/$1" >/dev/null; }

"$here/run-app.sh" >/dev/null || exit 1
curl -sf -XPOST "$c/dict" -d @"$F/dict.json" >/dev/null
set_ '{"stt":{"whisper_model":"ggml-base.en-q5_1.bin"},"language":"en"}' 3
want="${*:-1 2 3 4 5 6 7}"

if [[ " $want " == *" 1 "* ]]; then
  echo "== 1 custom words (event PTT, base.en)"
  r=$(dictate en_custom.wav); t=$(field "$r" .final_text)
  for w in Contaflow Supabase kubectl "Pablo Pérez" "Francisca Ugarte"; do check "1 $w" "$t" "$w"; done
  check "1 status" "$(field "$r" .status)" copied
fi

if [[ " $want " == *" 2 "* ]]; then
  echo "== 2 real PTT hotkey Ctrl+Alt+F20, Spanish, large-v3-turbo, language auto"
  set_ '{"stt":{"whisper_model":"ggml-large-v3-turbo-q5_0.bin"},"language":"auto"}' 5
  audio es_accents.wav; b=$(last_id)
  "$E" key down f20 ctrl,alt; sleep 0.6; check "2 hold -> recording" "$(phase)" recording
  sleep 4.8; "$E" key up f20 ctrl,alt
  r=$(wait_row "$b"); t=$(field "$r" .final_text)
  for w in reunión mañana "¿Puedes" facturación; do check "2 $w" "$t" "$w"; done
  r=$(dictate es_custom.wav); t=$(field "$r" .final_text)
  for w in "Francisca Ugarte" revisó Contaflow "Pablo Pérez"; do check "2 es dict $w" "$t" "$w"; done
  set_ '{"stt":{"whisper_model":"ggml-base.en-q5_1.bin"},"language":"en"}' 3
fi

if [[ " $want " == *" 3 "* ]]; then
  echo "== 3 hands-free hotkey, double tap, cancel"
  audio en_snippet.wav; b=$(last_id)
  "$E" key tap f20 ctrl,shift; sleep 0.6; check "3 toggle -> locked" "$(phase)" locked
  sleep 2.2; "$E" key tap f20 ctrl,shift
  r=$(wait_row "$b"); check "3 toggle row" "$(field "$r" .final_text)" "https://cal.example.com/pablo"
  audio en_snippet.wav; b=$(last_id)
  "$E" key tap f20 ctrl,alt; sleep 0.12; "$E" key tap f20 ctrl,alt; sleep 0.6
  check "3 double tap -> locked" "$(phase)" locked
  sleep 2.2; curl -sf -XPOST "$c/event/stop" >/dev/null
  r=$(wait_row "$b"); check "3 double tap row" "$(field "$r" .final_text)" "cal.example.com"
  audio en_custom.wav; b=$(last_id)
  "$E" key tap f20 ctrl,shift; sleep 1.5; "$E" key tap f20 alt,shift; sleep 1.5
  check "3 cancel -> idle" "$(phase)" idle
  check "3 cancel writes no row" "$(last_id)" "$b"
fi

if [[ " $want " == *" 4 "* ]]; then
  echo "== 4 text rules (cleanup none)"
  check "4 snippet whole utterance" "$(field "$(dictate en_snippet.wav)" .final_text)" "https://cal.example.com/pablo"
  t=$(field "$(dictate en_punct.wav)" .final_text)
  check "4 spoken 'new line'" "$t" $'\n'; check "4 spoken 'period'" "$t" "!period"; check "4 spoken 'question mark'" "$t" "!question mark"
  t=$(field "$(dictate en_scratch.wav)" .final_text)
  check "4 scratch that -> four" "$t" "4"; check "4 scratch that drops three" "$t" "!3"
  set_ '{"spoken_punctuation":false}'
  check "4 spoken punctuation off" "$(field "$(dictate en_punct.wav)" .final_text)" "period"
  set_ '{"spoken_punctuation":true}'
fi

if [[ " $want " == *" 5 "* ]]; then
  echo "== 5 gates"
  r=$(dictate silence.wav); check "5 silence: no row" "$(field "$r" .status)" no_row; check "5 silence message" "$(field "$r" .message)" "No audio"
  # PLAN 3.5: a clip under 250 ms is an accidental tap, dropped silently (after the 500 ms double-tap window).
  audio short.wav; b=$(last_id)
  curl -sf -XPOST "$c/event/hold_start" >/dev/null; sleep 0.15; curl -sf -XPOST "$c/event/hold_end" >/dev/null; sleep 2
  check "5 150 ms tap: no row" "$(last_id)" "$b"; check "5 150 ms tap: idle" "$(phase)" idle
fi

if [[ " $want " == *" 6 "* ]]; then
  echo "== 6 long dictation (60 s file, 46 s speech)"
  audio long.wav; b=$(last_id)
  curl -sf -XPOST "$c/event/hold_start" >/dev/null; sleep 46.5; curl -sf -XPOST "$c/event/hold_end" >/dev/null
  r=$(wait_row "$b"); t=$(field "$r" .final_text)
  check "6 status" "$(field "$r" .status)" copied
  for w in Contaflow kubectl Supabase "Pablo Pérez" Ugarte "have a nice day"; do check "6 $w" "$t" "$w"; done
  check "6 no trailing 'you'" "${t: -8}" "!you."
  echo "     latency_ms=$(field "$r" .latency_ms) chars=${#t}"
fi

if [[ " $want " == *" 7 "* ]]; then
  echo "== 7 real paste into TextEdit"
  if pgrep -x TextEdit >/dev/null; then
    echo "skip 7: TextEdit already runs (it may hold Pablo's docs)"
  else
    prev=$("$E" front | jq -r .bundle_id)
    clip=$(pbpaste | shasum)
    pid=$("$here/lib/textedit.sh" open "$doc"); sleep 1.5
    if [ "$("$E" front | jq -r .bundle_id)" != com.apple.TextEdit ]; then
      fails=$((fails + 1)); echo "FAIL 7 TextEdit did not come to the front"
    else
      r=$(dictate en_snippet.wav); sleep 0.6
      v=$("$E" axvalue "$pid" "$(basename "$doc")" 2>&1)
      check "7 status pasted" "$(field "$r" .status)" pasted
      check "7 TextEdit has the text" "$v" "https://cal.example.com/pablo"
      curl -sf -XPOST "$c/event/paste_last" >/dev/null; sleep 1
      v=$("$E" axvalue "$pid" "$(basename "$doc")" 2>&1)
      check "7 paste last pasted again" "$(grep -o 'cal.example.com' <<<"$v" | wc -l | tr -d ' ')" 2
      check "7 clipboard restored" "$(pbpaste | shasum)" "$clip"
    fi
    "$here/lib/textedit.sh" close "$doc"; rm -f "$doc"
    [ -n "$prev" ] && [ "$prev" != null ] && open -b "$prev"
  fi
fi

echo "pass $pass, fail $fails"
[ $fails = 0 ]
