#!/usr/bin/env bash
# Build an optimized binary and install it for the current user.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo build --release -p accela
install -Dm755 target/release/accela "$HOME/.local/bin/accela"
apps="$HOME/.local/share/applications"
mkdir -p "$apps"
cat > "$apps/accela.desktop" <<'DESKTOP'
[Desktop Entry]
Type=Application
Name=accela
Comment=Fast GPU terminal
Exec=accela
Icon=utilities-terminal
Terminal=false
Categories=System;TerminalEmulator;
StartupWMClass=accela
DESKTOP
echo "installed: $HOME/.local/bin/accela ($(accela --version 2>/dev/null || echo 'not on PATH'))"
