#!/usr/bin/env bash
# Build an optimized binary and install it for the current user.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p lain
install -Dm755 target/release/lain "$HOME/.local/bin/lain"
apps="$HOME/.local/share/applications"
mkdir -p "$apps"
cat > "$apps/lain.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=lain
Comment=Fast GPU terminal
Exec=lain
Icon=utilities-terminal
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=lain
DESKTOP
echo "installed: $HOME/.local/bin/lain ($(lain --version 2>/dev/null || echo 'not on PATH'))"
