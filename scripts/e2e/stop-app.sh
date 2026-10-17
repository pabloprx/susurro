#!/bin/bash
# Stops the app started by run-app.sh. E2E_DIR as in run-app.sh. --clean also deletes the data dir.
set -uo pipefail
dir="${E2E_DIR:-/tmp/susurro-e2e}"
if [ -f "$dir/app.pid" ]; then
  pid=$(cat "$dir/app.pid")
  kill "$pid" 2>/dev/null
  for _ in $(seq 1 30); do kill -0 "$pid" 2>/dev/null || break; sleep 0.2; done
  kill -9 "$pid" 2>/dev/null
  rm -f "$dir/app.pid"
  echo "stopped $pid"
fi
case "$dir" in /tmp/*|/private/tmp/*) [ "${1:-}" = "--clean" ] && rm -rf "$dir" ;; esac
exit 0
