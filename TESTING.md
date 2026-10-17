# Testing susurro

First, set the PATH: `export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"`.
The models are in `~/.cache/susurro/`: `ggml-base.en-q5_1.bin`, `ggml-base.bin`, `ggml-large-v3-turbo-q5_0.bin`.
Every app suite uses its own data dir under `/tmp` and the Keychain service `app.susurro.desktop.e2e`. No suite touches your real susurro data.

## Suites, one command each

| # | Command | Time | What it covers |
|---|---------|------|----------------|
| 1 | `bun run check` | ~1 min | The IPC contract check (`contract.gen.ts` is fresh). 168 Rust unit tests: pipeline, session state machine, text rules, dictionary, providers against mockito, device resolution. 18 dictionary property tests (proptest). Typecheck and the frontend build. |
| 2 | `bun run test:ui` | ~50 s | 96 Playwright tests on the React UI in mock mode: every screen, onboarding, overlay states. |
| 3 | `SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-base.bin cargo test --release --manifest-path src-tauri/Cargo.toml --test whisper_smoke --test whisper_edge -- --ignored --nocapture --test-threads=1` | ~2 min | Real whisper with Metal: jfk.wav, empty, short and noise clips, Spanish, language auto. |
| 4 | `scripts/e2e/providers-real.sh` | ~2 min | The WER of every fixture on every local model. whisper-server as an OpenAI-compatible STT server. Clear messages when a server is down. With keys in the env, also the real ElevenLabs and OpenRouter calls (see below). |
| 5 | `scripts/e2e/devices-real.sh` | ~2 min | 32 checks of mic hot-plug on the real app, with CoreAudio aggregate devices: new-device prompt, switch, keep, fallback, restore, twins, priority, a mic lost during a dictation. |
| 6 | `scripts/e2e/scenarios.sh [1-7 ...]` | ~4 min | 44 checks of the full app with fixture audio (see below). |
| 7 | `scripts/e2e/smoke.sh` | ~20 s | One dictation from start to stop. Use it to check that the harness works. |
| 8 | `scripts/e2e/real-mic.sh` | ~5 min | The real microphone. It needs a human (see below). |

Suites 5-7 build the debug binary first. Set `E2E_SKIP_BUILD=1` to use the last build.
Suite 4, cloud part: `set -a; source ~/projects/voice-ui/.env; source ~/projects/dictate/.env; set +a; scripts/e2e/providers-real.sh`. Each cloud test makes real calls (ElevenLabs 2, OpenRouter 5) and costs money.

## The app scenarios (`scenarios.sh`)

1. Custom words: a push-to-talk dictation from the control endpoint. The dictionary terms come out with their stored spelling.
2. The real push-to-talk hotkey (Ctrl+Alt+F20, posted with CGEvent). Spanish on large-v3-turbo with language auto: accents, `¿`, dictionary terms.
3. Hands-free: the toggle hotkey, a double tap of the push-to-talk key, and the cancel hotkey (no history row).
4. Text rules with no LLM cleanup: a snippet, spoken punctuation on and off, "scratch that".
5. Gates: silence gives "No audio from ...". A 150 ms tap gives no history row.
6. A long dictation (46 s of speech): all the words, and no "you" hallucination at the end.
7. A real paste into a TextEdit doc that the script opens: Cmd+V, paste-last, and the clipboard comes back as it was. The script skips this scenario if TextEdit already runs. It brings TextEdit to the front for about 5 s.

## The harness

`scripts/e2e/run-app.sh` builds and launches the debug app, isolated. `stop-app.sh` stops it.
Debug builds compile `src-tauri/src/test_hooks.rs`. Release builds do not.
The control endpoint is on `http://127.0.0.1:$E2E_PORT`:
- `GET /state`, `GET /permissions`
- `POST /settings`, `/dict`, `/audio`, `/event/<hold_start|hold_end|toggle|stop|cancel|paste_last>`
- `POST /permissions/request/<microphone|accessibility|input_monitoring>`

The source code of each script lists more routes and env vars.

## What still needs a human

`scripts/e2e/real-mic.sh` tests the one path that the other suites cannot: real sound into a real microphone.
The terminal has no Microphone grant, so cpal records zeros in a binary launched from the shell. This script builds a debug `.app` with its own identity (`app.susurro.desktop.e2e`, product name `susurro-e2e`) and launches it with `open`. Your real susurro and its grants stay as they are.

1. Quit TextEdit.
2. Turn up the speakers. Put the mic near them.
3. Run `scripts/e2e/real-mic.sh`.
4. When macOS asks, allow Microphone for susurro-e2e.
5. In System Settings > Privacy & Security > Accessibility, turn on susurro-e2e.
6. Do not touch the Mac while the fixture plays. The script plays it with `afplay` while it dictates into its own TextEdit doc.
7. Read the result. PASS means the history status is `pasted` and the doc has at least 70% of the fixture's words.
8. To remove the grants later, run `scripts/e2e/real-mic.sh --reset`.

`scripts/e2e/real-mic.sh --check` builds and launches the `.app` and reads its permissions. It shows no prompts and plays no audio.

These items also need a human:
- The fn key. The tests never post fn events, because the system owns fn. Hold fn in TextEdit, Slack and a terminal.
- The look of the overlay on the real screen, and the system prompts in onboarding from a fresh signed build.

## Known limits (2026-10-17)

- base.en writes "Francisco Ugarte" in `long.wav`. The dictionary does not change it on purpose: a fuzzy match must not flip a gendered name.
- large-v3-turbo writes "en su pavase" for "en Supabase" in Spanish. The dictionary does not glue a common word ("su") to a split word, on purpose.
- Posted hotkey chords work only on F20 on this Mac (not F13-F19).
- If `tauri build` fails in whisper-rs-sys with "'path' is unavailable: introduced in macOS 10.15", the CMake cache is from the old deployment target. Run `cargo clean --manifest-path src-tauri/Cargo.toml -p whisper-rs-sys` once (add `--target-dir` for the e2e target dirs).
