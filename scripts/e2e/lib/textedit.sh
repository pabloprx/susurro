#!/bin/bash
# Test target for pastes. open <file> [--bg]: opens the file in TextEdit (frontmost unless --bg), prints the pid.
# close <file>: closes that window only; quits TextEdit only if `open` started it. Never touch other TextEdit docs.
set -euo pipefail
d="$(cd "$(dirname "$0")" && pwd)"
mark="/tmp/susurro-e2e-textedit-started"
case "${1:-}" in
  open)
    f="$2"; [ -f "$f" ] || : > "$f"
    pgrep -x TextEdit >/dev/null || touch "$mark"
    if [ "${3:-}" = "--bg" ]; then open -g -a TextEdit "$f"; else open -a TextEdit "$f"; fi
    for _ in $(seq 1 50); do "$d/e2e" windows TextEdit | grep -q "\"$(basename "$f")\"" && break; sleep 0.1; done
    pgrep -x TextEdit ;;
  close)
    pid=$(pgrep -x TextEdit) || exit 0
    "$d/e2e" axclose "$pid" "$(basename "$2")" || true
    if [ -f "$mark" ]; then
      sleep 0.5
      if [ "$("$d/e2e" windows TextEdit)" = "[]" ]; then kill "$pid"; fi
      rm -f "$mark"
    fi ;;
  *) echo "textedit.sh open <file> [--bg] | close <file>" >&2; exit 1 ;;
esac
