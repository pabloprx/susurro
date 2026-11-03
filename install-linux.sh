#!/usr/bin/env bash
# Linux: build susurro and install it for this user (dictation only; voice control is macOS-only).
#   ./install-linux.sh           build + install to ~/.local/bin, add a menu entry, start it
#   ./install-linux.sh --deps    install the system packages first (apt, dnf or pacman; asks for sudo)
#   ./install-linux.sh --no-open build + install, do not start it
# Needs: Rust (rustup), bun, and the WebKitGTK/ALSA dev packages (--deps installs them).
# Paste: X11 needs xdotool. Wayland needs wtype (wlroots, KDE) or ydotool (GNOME, needs its daemon).
set -euo pipefail

DEPS=0
OPEN=1
for a in "$@"; do
  case "$a" in
    --deps) DEPS=1 ;;
    --no-open) OPEN=0 ;;
    -h|--help) sed -n '2,7p' "$0"; exit 0 ;;
    *) echo "unknown flag: $a" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")"

if [ "$DEPS" = 1 ]; then
  if command -v apt-get >/dev/null; then
    sudo apt-get update
    sudo apt-get install -y build-essential cmake clang pkg-config curl libwebkit2gtk-4.1-dev libgtk-3-dev \
      libayatana-appindicator3-dev librsvg2-dev libasound2-dev libssl-dev xdotool wtype
  elif command -v dnf >/dev/null; then
    sudo dnf install -y gcc gcc-c++ cmake clang pkgconf-pkg-config curl webkit2gtk4.1-devel gtk3-devel \
      libappindicator-gtk3-devel librsvg2-devel alsa-lib-devel openssl-devel xdotool wtype
  elif command -v pacman >/dev/null; then
    sudo pacman -S --needed --noconfirm base-devel cmake clang pkgconf curl webkit2gtk-4.1 gtk3 \
      libayatana-appindicator librsvg alsa-lib openssl xdotool wtype
  else
    echo "Unknown package manager: install WebKitGTK 4.1, GTK 3, ALSA, OpenSSL, cmake, clang and xdotool/wtype yourself." >&2
  fi
fi

command -v cargo >/dev/null || { echo "Rust is missing: curl https://sh.rustup.rs -sSf | sh" >&2; exit 1; }
command -v bun >/dev/null || { echo "bun is missing: curl -fsSL https://bun.sh/install | bash" >&2; exit 1; }

bun install
# --no-bundle: one binary is all we need; the .deb/.rpm/AppImage come from `bun tauri build`
bun tauri build --no-bundle

BIN="$HOME/.local/bin"
APPS="$HOME/.local/share/applications"
ICONS="$HOME/.local/share/icons/hicolor/128x128/apps"
mkdir -p "$BIN" "$APPS" "$ICONS"
pkill -x susurro 2>/dev/null || true
install -m 755 src-tauri/target/release/susurro "$BIN/susurro"
install -m 644 src-tauri/icons/128x128.png "$ICONS/susurro.png"
cat > "$APPS/susurro.desktop" <<EOF
[Desktop Entry]
Type=Application
Name=susurro
Comment=Hold a key, talk, and the text lands where you type
Exec=$BIN/susurro
Icon=susurro
Categories=Utility;Accessibility;
Terminal=false
EOF
echo "Installed $BIN/susurro"

if [ "${XDG_SESSION_TYPE:-}" = wayland ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
  command -v wtype >/dev/null || command -v ydotool >/dev/null \
    || echo "Note: install wtype or ydotool so susurro can paste. Until then text goes to the clipboard."
  echo "Note: Wayland does not let apps listen for a global key, so the talk key only works while an X11 app is focused."
  echo "      For now, pick the X11 session at login (\"GNOME on Xorg\", \"Plasma (X11)\")."
else
  command -v xdotool >/dev/null || echo "Note: install xdotool so susurro can paste. Until then text goes to the clipboard."
fi

if [ "$OPEN" = 1 ]; then
  nohup "$BIN/susurro" >/dev/null 2>&1 &
  echo "susurro is running. Hold F1 and talk."
fi
