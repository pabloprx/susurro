# susurro

System-wide voice dictation for macOS and Linux: hold a key, speak, and the text is pasted into whatever app you are in. On macOS it also has an optional voice-control mode that drives your Mac by voice ([Linux](#linux) is dictation only). Plan: `PLAN.md`. UI spec: `DESIGN.md`.

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

## Linux

Dictation works on Linux: talk key, recording, local Whisper (CPU) or the cloud providers, cleanup, dictionary, history and paste. Voice control, agent mode and modifier-only talk keys (Fn, Right Option) are macOS-only.

```sh
git clone https://github.com/pabloprx/susurro && cd susurro
./install-linux.sh --deps   # first time: installs WebKitGTK, ALSA, cmake, xdotool/wtype (apt, dnf or pacman)
./install-linux.sh          # later: rebuild and reinstall
```

It installs `~/.local/bin/susurro` and a menu entry, then starts it. Hold F1 and talk. Settings, keys and models live in `~/.local/share/app.susurro.desktop`. `bun tauri build` makes a .deb, .rpm and AppImage instead.

- **X11:** works fully. Paste needs `xdotool`. Terminals get Ctrl+Shift+V, every other app Ctrl+V.
- **Wayland:** paste works with `wtype` (wlroots compositors, KDE) or `ydotool` (GNOME; start `ydotoold` first). The talk key does not: Wayland lets no app listen for a global key, so it fires only while an X11 app is focused. Pick the X11 session at login for now.
- No typing helper installed: each dictation lands on the clipboard and susurro tells you so once at startup.
- Local Whisper runs on the CPU. Pick a small model (base or small) on older machines.

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

## Setup guide: pick how it transcribes

Choose one. You can switch any time in **Providers**, and you can mix them (local Whisper as the fallback when the network is down).

### Option A: local Whisper (free, private, offline)

1. Finish onboarding. It downloads the quick-start model (`base`, 60 MB) and then the recommended one (`large-v3-turbo q5_0`, about 575 MB) in the background.
2. Open **Providers**, pick **Local Whisper**, and choose the model. Use a multilingual model if you speak anything other than English: the `.en` models turn other languages into English.
3. Hold **F1** in any text field, speak, release. Done. No account, no key, nothing leaves your Mac.

### Option B: OpenRouter (cloud, best accuracy, needs a key)

1. Create a key at [openrouter.ai/keys](https://openrouter.ai/keys) and add a few dollars of credit.
2. Open **Providers**, paste it into the **OpenRouter** key row at the bottom, and save. Keys are stored only on your Mac, in `keys.json` (mode 600).
3. Pick **OpenRouter** as the transcription provider and choose a model (MAI-Transcribe is the default). Press **Test** to check it.
4. Optional: in **Cleanup**, choose OpenRouter to have an LLM fix punctuation and filler words.

ElevenLabs and OpenAI keys work the same way in Providers if you prefer their realtime transcription.

### Voice control and the desktop agent (optional)

The agent drives your Mac by voice: open apps, click, type, search, copy text from the screen, send messages (it asks before anything risky).

1. Install [Claude Code](https://claude.com/claude-code) and log in once in a terminal: run `claude` and follow the login. The agent uses your own Claude subscription; susurro stores no Anthropic key.
2. Grant **Screen Recording** and **Automation** when asked (System Settings > Privacy & Security).
3. Open **Voice Control**, turn it on, and check that **Agent mode** is on. **Computer use** (on by default) makes it look at the whole screen after each action.
4. Hold the voice-control key (**F2** by default, change it in Voice Control), say an order, release. Try "abre Safari y busca recetas de pasta" or "open YouTube and search lo-fi music". Press **Esc** or say "stop" to stop it.

Without Claude Code installed or logged in, the agent falls back to OpenRouter (it needs the OpenRouter key from Option B).

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
