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

## Streaming transcription (local whisper)

While you hold the key, `audio::Chunker` cuts the audio at pauses (350 ms after at least 3 s, a hard cut at the quietest 100 ms after 20 s). `whisper_local::Stream` transcribes each chunk on one worker thread, with the previous chunk's last words as the prompt. At the release only the tail is left. Cloud providers stay single-shot (a chunked upload was not measured: it costs money and needs keys).

Bench: `SUSURRO_WHISPER_THREADS=6 cargo run --release --manifest-path src-tauri/Cargo.toml --example stream_bench -- ~/.cache/susurro/ggml-large-v3-turbo-q5_0.bin`. It feeds the audio in real time (100 ms pieces, like the session tick) and times the release to the final STT text. The cleanup LLM and the paste are not in it (they are the same for both modes).

Measured 2026-10-17, Mac Studio M4 Max, large-v3-turbo q5_0, 6 threads, another release build compiling at the same time:

| Clip | Audio | Mode | Chunks | Release to text | WER |
|------|-------|------|--------|-----------------|-----|
| long.wav, 14 s padding trimmed | 46.6 s | single-shot | 1 | 888 ms | 0.033 |
| long.wav, 14 s padding trimmed | 46.6 s | streaming | 3 | 244 ms | 0.026 |
| that clip x3, 0.6 s gaps | 140.9 s | single-shot | 1 | 2644 ms | 0.033 |
| that clip x3, 0.6 s gaps | 140.9 s | streaming | 9 | 241 ms | 0.026 |

The word lists are the same length in both modes (151 and 453 words): no word lost or doubled at a cut. The one difference is "Ugart" (single-shot) against "Ugarte" (streaming). One run each: the numbers are not a median.

Re-run 2026-10-17 (review), same Mac and command, idle machine, 3 runs: median release to text 874 / 260 ms (long.wav single / streaming) and 2664 / 252 ms (x3). Same WER and chunk counts. The app adds the hands-free grace (default 400 ms, checked on the 100 ms tick) before the STT starts; the bench does not include it.

## Latency per STT path (next-queue 5)

`src-tauri/tests/latency_bench.rs` plays each e2e fixture at real-time pace (100 ms of audio every 100 ms, as the session tick sends it) and times the release to the final STT text. The cleanup LLM and the paste are not in it: they are the same for every path.

```
SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-large-v3-turbo-q5_0.bin SUSURRO_WHISPER_THREADS=6 OPENROUTER_API_KEY=.. ELEVENLABS_API_KEY=.. \
  cargo test --release --manifest-path src-tauri/Cargo.toml --test latency_bench -- --ignored --nocapture --test-threads=1
```

Measured 2026-10-17, Mac Studio M4 Max, home network, median of 3 (the run values in brackets):

| Path | en_snippet (2.0 s) | es_accents (4.9 s) | en_numbers (8.7 s) |
|------|-------------------|--------------------|--------------------|
| Local turbo streaming (large-v3-turbo q5_0, 6 threads) | 354 ms [354, 351, 373] | 369 ms [404, 369, 357] | 400 ms [400, 377, 432] |
| MAI-Transcribe 2 batch (OpenRouter) | 695 ms [1692, 695, 689] | 737 ms [752, 735, 737] | 768 ms [1259, 768, 755] |
| Scribe v2 batch (ElevenLabs) | 590 ms [590, 616, 565] | 646 ms [813, 646, 571] | 1005 ms [1065, 1005, 668] |
| Scribe v2 Realtime (WebSocket, commit at the release) | 198 ms [193, 209, 198] | 210 ms [205, 219, 210] | 208 ms [191, 208, 257] |

Text quality on these clips: all four paths got every word. Scribe batch wrote "five five five zero one nine nine" for "555-0199"; Scribe Realtime wrote "a las 3:00 p.m." for "a las 3" (an added "p.m."). One sample each, so these are anecdotes, not a WER.

Voice control cuts (`command_chunks`, the COMMAND cuts, one run): Scribe Realtime answers a commit 197-259 ms after the cut (3 cuts). MAI answers a cut chunk in 724-2116 ms (3 chunks; 2116 ms was a 7 s chunk).

Defaults from these numbers (Pablo's explicit choices stay as they are):
- Voice control: `command.stt` = auto = Scribe Realtime when an ElevenLabs key is saved (about 3x faster per order than MAI), else the dictation provider. Partials show in the pill; each commit (a 300 ms pause) goes to the planner. If the socket fails twice, the unanswered audio and the rest of the session go to MAI (or Scribe batch).
- Dictation: the default stays Local Whisper streaming (0.35-0.4 s, free, offline, no key at install). Scribe Realtime (0.2 s) is the fastest choice when an ElevenLabs key is saved; MAI (0.7 s) is the slowest of the four. A failed realtime stream runs the clip single-shot on MAI (or Scribe batch).
- Not measured: the ElevenLabs realtime price per hour, long dictations over 36 s on Scribe Realtime (the server commits by itself there), and the release to paste in the real app (no GUI test while Pablo uses the Mac).

## Known limits (2026-10-17)

- base.en writes "Francisco Ugarte" in `long.wav`. The dictionary does not change it on purpose: a fuzzy match must not flip a gendered name.
- large-v3-turbo writes "en su pavase" for "en Supabase" in Spanish. The dictionary does not glue a common word ("su") to a split word, on purpose.
- Posted hotkey chords work only on F20 on this Mac (not F13-F19).
- If `tauri build` fails in whisper-rs-sys with "'path' is unavailable: introduced in macOS 10.15", the CMake cache is from the old deployment target. Run `cargo clean --manifest-path src-tauri/Cargo.toml -p whisper-rs-sys` once (add `--target-dir` for the e2e target dirs).

## Agent v2 (voice control)

Every F11 order goes to the agent (`agent.rs`): Pablo's `claude -p`, one warm stream-json process, susurro's MCP tools. Trivial orders run with no LLM (`agent::local_intent`). The planner path is the setting "Fast mode (beta)".

Unit tests (in `bun run check`): a fake desk and a fake `claude` script (Python). They cover the warm process across orders, the rolling memory in a new process, the mouse tools (a JS Paint line is a drag), the take-over stop, the local intents and the v8 settings migration.

Real CLI, fake desk (no clicks): `SMOKE_MODEL=sonnet cargo test --manifest-path src-tauri/Cargo.toml --lib agent::tests::smoke_real_claude -- --ignored --nocapture`.

The test hook for GUI tests (debug builds, `SUSURRO_TEST_CONTROL`):
- `POST /command {"text": "...", "auto_confirm": true}` runs an order as if heard after F11 (no mic, no STT). `auto_confirm` answers every confirm with yes.
- `GET /state` has `command_log` (the steps), `live`, `agent` (the warm process) and `agent_memory`.
- `SUSURRO_TEST_PASTE_TARGET` takes a list: `com.docker.docker,com.electron.dockerdesktop`.

Real GUI runs, 2026-10-18, one run each (not a median), from POST /command to the session end, Spanish orders. Every run passed.

| Task | sonnet | haiku |
|------|--------|-------|
| JS Paint: draw a line (new process) | 3.29 s | 5.55 s |
| JS Paint: "draw the same again, moved" (same process: it remembered the first line) | 2.88 s | 5.14 s |
| Finder: new folder in ~/susurro-tests | 4.93 s | 5.13 s |
| Slack Automatization: type and send | 3.28 s | 3.91 s |
| Docker Desktop: create a volume | 7.04 s | 5.59 s |
| Total | 21.42 s | 25.32 s |

The default model stays sonnet. The fake-desk smoke gave the same order: sonnet 1.8 s and 1.0 s per order, haiku 2.3 s and 7.4 s.
An action can end the order (`done` on the action): this saved one model round trip, about 1.5 s per order (smoke: 3.2 s -> 1.8 s).

Command STT: local Whisper large-v3-turbo q5_0, 6 threads, `whisper_bench` on 4 short Spanish orders (0.8-2.4 s of audio): 318-328 ms each, warm. One error: "prueba del agente" -> "prueba de la gente".
