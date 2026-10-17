#!/bin/bash
# Builds the debug binary (embedded frontend, test hooks on) and launches it directly, isolated.
# Env (all optional):
#   E2E_DIR=/tmp/susurro-e2e     data dir (settings, db, audio, logs, models -> symlinks to ~/.cache/susurro)
#   E2E_PORT=47913               control port (http://127.0.0.1:$E2E_PORT)
#   E2E_AUDIO=<wav[:wav...]>     test audio queue (default fixtures/en_custom.wav)
#   E2E_PASTE_TARGET=com.apple.TextEdit
#   E2E_SKIP_BUILD=1             reuse the last build
# Prints the pid. Logs: $E2E_DIR/app.log (stdout) and $E2E_DIR/logs/susurro.log.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
dir="${E2E_DIR:-/tmp/susurro-e2e}"
port="${E2E_PORT:-47913}"
audio="${E2E_AUDIO:-$here/fixtures/en_custom.wav}"
target="${E2E_PASTE_TARGET:-com.apple.TextEdit}"
bin="$root/src-tauri/target/e2e/debug/susurro"

case "$dir" in /tmp/*|/private/tmp/*) ;; *) echo "E2E_DIR must be under /tmp" >&2; exit 1 ;; esac
if [ -f "$dir/app.pid" ] && kill -0 "$(cat "$dir/app.pid")" 2>/dev/null; then
  echo "already running: pid $(cat "$dir/app.pid") (stop-app.sh first)" >&2; exit 1
fi
[ -f "$here/fixtures/en_custom.wav" ] || "$here/fixtures/make.sh" >/dev/null

if [ -z "${E2E_SKIP_BUILD:-}" ]; then
  (cd "$root" && bun run build >/dev/null 2>&1) || { echo "frontend build failed" >&2; exit 1; }
  # Own target dir: the custom-protocol build does not thrash the plain `cargo test` cache.
  cargo build --manifest-path "$root/src-tauri/Cargo.toml" --features tauri/custom-protocol --target-dir "$root/src-tauri/target/e2e" 2>&1 | grep -E "^(error|warning: unused)" >&2 || true
fi
[ -x "$bin" ] || { echo "build failed: $bin missing" >&2; exit 1; }

mkdir -p "$dir/models"
for m in "$HOME"/.cache/susurro/*.bin; do [ -e "$m" ] && ln -sf "$m" "$dir/models/"; done
# Seed: onboarded (no main window), language en (the seeded model is English-only: "auto" fails every dictation), harmless hotkeys (never fn: Pablo's real app owns it), no LLM cleanup.
[ -f "$dir/settings.json" ] || cat > "$dir/settings.json" <<'JSON'
{"onboarded": true, "language": "en",
 "hotkeys": {"ptt": "Ctrl+Alt+F20", "hands_free": "Ctrl+Shift+F20", "double_tap": true, "cancel": "Alt+Shift+F20", "paste_last": "Ctrl+Cmd+F20"},
 "stt": {"provider": "whisper_local", "whisper_model": "ggml-base.en-q5_1.bin"},
 "cleanup": {"level": "none"},
 "recording": {"sounds": false}}
JSON

SUSURRO_DATA_DIR="$dir" SUSURRO_TEST_AUDIO="$audio" SUSURRO_TEST_PASTE_TARGET="$target" SUSURRO_TEST_CONTROL="$port" \
  nohup "$bin" >"$dir/app.log" 2>&1 &
pid=$!
echo "$pid" >"$dir/app.pid"
for _ in $(seq 1 100); do
  curl -sf "http://127.0.0.1:$port/state" >/dev/null 2>&1 && { echo "$pid"; exit 0; }
  kill -0 "$pid" 2>/dev/null || { echo "app exited, see $dir/app.log" >&2; tail -20 "$dir/app.log" >&2; exit 1; }
  sleep 0.3
done
echo "control port $port did not answer in 30 s" >&2; exit 1
