# susurro

System-wide voice dictation for macOS. Plan: `PLAN.md`. UI spec: `DESIGN.md`.

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
