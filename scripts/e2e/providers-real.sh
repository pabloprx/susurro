#!/bin/bash
# Real-provider suite (src-tauri/tests/providers_real.rs). Starts whisper-server (-l auto, ggml-base.bin) on a free
# port, runs the tests, stops it. Cloud tests run only when their key is in the env (never the Keychain):
#   set -a; source ~/projects/voice-ui/.env; source ~/projects/dictate/.env; set +a; scripts/e2e/providers-real.sh
# Env: SUSURRO_TEST_MODELS (default: every ~/.cache/susurro/*.bin), SUSURRO_TEST_OR_STT, SUSURRO_TEST_OR_CHAT.
set -uo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
log=$(mktemp /tmp/susurro-whisper-server.XXXX)
/opt/homebrew/bin/whisper-server -m "$HOME/.cache/susurro/ggml-base.bin" -l auto --host 127.0.0.1 --port "$port" \
  --inference-path /v1/audio/transcriptions >"$log" 2>&1 &
ws=$!
trap 'kill $ws 2>/dev/null; rm -f "$log"' EXIT
for _ in $(seq 1 50); do curl -s -o /dev/null "http://127.0.0.1:$port/" && break; sleep 0.2; done

export SUSURRO_TEST_OAI_BASE="http://127.0.0.1:$port/v1"
export SUSURRO_TEST_MODELS="${SUSURRO_TEST_MODELS:-$(ls "$HOME"/.cache/susurro/*.bin | paste -sd: -)}"
tests=(whisper_wer openai_compat_server stt_server_down_message local_llm_down)
[ -n "${ELEVENLABS_API_KEY:-}" ] && tests+=(elevenlabs_real) || echo "skip elevenlabs_real: no ELEVENLABS_API_KEY"
[ -n "${OPENROUTER_API_KEY:-}" ] && tests+=(openrouter_real) || echo "skip openrouter_real: no OPENROUTER_API_KEY"
rc=0
for t in "${tests[@]}"; do
  echo "=== $t"
  cargo test --release --manifest-path "$here/../../src-tauri/Cargo.toml" --test providers_real "$t" -- --ignored --nocapture --exact 2>&1 \
    | grep -vE "^(whisper_|ggml_|load_backend|warning|  = |  \||\s*Compiling|\s*Finished|\s*Running)" || rc=1
done
exit $rc
