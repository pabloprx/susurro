# susurro

System-wide voice dictation for macOS: hold a key, speak, and the text is pasted into whatever app you are in. It also has an optional voice-control mode that drives your Mac by voice. Plan: `PLAN.md`. UI spec: `DESIGN.md`.

## Requirements

- macOS 11 or later. Apple Silicon is recommended (local Whisper runs on Metal).
- Xcode Command Line Tools: `xcode-select --install`
- Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- Bun and CMake (CMake builds whisper.cpp): `brew install oven-sh/bun/bun cmake`

## Install (build from source)

```sh
git clone https://github.com/pabloprx/susurro && cd susurro
./install.sh
```

`install.sh` builds the release app, signs it, copies it to `/Applications/susurro.app` and opens it. The first build takes several minutes. Run it again to update after a `git pull`; `./install.sh --fresh` wipes settings, history, keys and permission grants first.

### Signing

macOS ties the permission grants to the app's signature. `install.sh` picks, in order: `SUSURRO_SIGNING_IDENTITY`, the identity in `src-tauri/tauri.conf.json` if your Mac has it, your first "Apple Development" identity, or an ad hoc signature.

- With an Apple Development identity, grants survive rebuilds. A free one works: sign in to Xcode with your Apple ID (Xcode > Settings > Accounts > Manage Certificates > +). List yours with `security find-identity -v -p codesigning`.
- Ad hoc also works, but macOS asks for the permissions again after every rebuild.

## First run

Onboarding walks you through these permissions (System Settings > Privacy & Security):

| Permission | Why |
| --- | --- |
| Microphone | Record while you hold the key. |
| Accessibility | Paste the text (Cmd+V) into the focused app. Without it the text stays on the clipboard. |
| Input Monitoring | The global hotkeys. macOS may ask you to relaunch susurro after granting it. |
| Screen Recording | Voice control only: read the screen to find what to click. |
| Automation | Voice control only: read and click links in your browser. |

Onboarding also downloads a small Whisper model (60 MB) to start right away, then the recommended one (about 575 MB) in the background. Switch models in Providers. Models, settings, history and keys live in `~/Library/Application Support/app.susurro.desktop/`.

Default keys: hold **F1** to dictate, double-tap for hands-free, **Ctrl+F1** for recent dictations. Change them in Shortcuts.

## API keys (all optional)

Dictation works fully offline with local Whisper; no key is needed. Keys unlock cloud providers, entered in the Providers screen:

| Key | Unlocks | Get one |
| --- | --- | --- |
| OpenRouter | Cloud transcription (MAI-Transcribe), LLM text cleanup, the voice-control planner | openrouter.ai/keys |
| ElevenLabs | Scribe transcription, including realtime | elevenlabs.io (Developers > API keys) |
| OpenAI | OpenAI realtime transcription | platform.openai.com/api-keys |
| Local STT / LLM | Your own OpenAI-compatible server (LM Studio, llama.cpp) | none |

The voice agent (Voice Control > Agent mode) runs your local [Claude Code](https://claude.com/claude-code) CLI (`claude -p`) under your own Claude login; no Anthropic API key is stored in susurro. Set `SUSURRO_CLAUDE` if `claude` is not in `~/.local/bin`, `/opt/homebrew/bin` or `/usr/local/bin`.

## Dev commands

```sh
export PATH="$HOME/.cargo/bin:/opt/homebrew/bin:$PATH"
bun install
bun run dev          # UI only, in a browser with mocks: http://localhost:1420 and /overlay
bun tauri dev        # the real app (permission grants go to the terminal)
bun run check        # cargo test (also regenerates src/lib/contract.gen.ts) + typecheck + build
```

End-to-end check with the real modules (jfk.wav -> pipeline with whisper metal + dictionary -> paste.rs into a temp .txt in TextEdit -> Cmd+S -> read the file back):

```sh
mkdir -p ~/.cache/susurro && test -f ~/.cache/susurro/ggml-base.en-q5_1.bin || curl -L -o ~/.cache/susurro/ggml-base.en-q5_1.bin https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en-q5_1.bin
SUSURRO_TEST_MODEL=~/.cache/susurro/ggml-base.en-q5_1.bin cargo run --release --manifest-path src-tauri/Cargo.toml --example e2e
```

The terminal needs Accessibility (Cmd+V, Cmd+S). Add `-- --mic 5` to record 5 s from the resolved mic instead.

Unit tests per area: `cargo test --lib -- pipeline:: history:: session::` (cargo takes several filters only after `--`).

Signed debug bundle (for permission testing): `bun tauri build --debug --bundles app && open src-tauri/target/debug/bundle/macos/susurro.app`.

IPC types live in `src-tauri/src/contract.rs`. Never edit `src/lib/contract.gen.ts` by hand: run `cargo test`.

## Manual checklist (needs a human, run on the signed debug bundle)

1. Hold fn in TextEdit, Slack and a terminal. Say "Contaflow uses Tauri, new line, thanks". Expect the dictionary spelling, and the clipboard keeps its earlier contents.
2. Double-tap fn: hands-free starts. Tap fn again: it stops and pastes.
3. Press Esc while recording: "Cancelled" toast. Click Undo within 5 s: the clip is processed.
4. Unplug and replug a mic during a session ("Mic lost - continuing on ...") and while idle ("... disconnected - using ...", "Using ... again").
5. Paste into Slack, Cursor and a Chrome text field.
6. Remove Accessibility: the pill shows "copied" and the text stays on the clipboard.
7. Copy an image, dictate, then paste: the image is still on the clipboard.
8. Pick a cloud STT with no network: the toast says local whisper was used.
9. From a fresh signed build, grant all three permissions in onboarding.
10. Tray: Microphone submenu switches devices, Paste last transcript, Pause hotkey, Quit.

## API keys

Never commit API keys. susurro stores keys in `~/Library/Application Support/app.susurro.desktop/keys.json` (mode 600), outside the repo. Tests read keys from env vars only (`OPENROUTER_API_KEY`, `ELEVENLABS_API_KEY`).
