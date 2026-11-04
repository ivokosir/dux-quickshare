#!/usr/bin/env bash
# Builds qsbar, installs the user service and the DankMaterialShell plugin.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release
install -Dm755 target/release/qsbar "$HOME/.local/bin/qsbar"
install -Dm644 systemd/qsbar.service "$HOME/.config/systemd/user/qsbar.service"
install -Dm644 assets/qsbar.svg "$HOME/.local/share/icons/hicolor/scalable/apps/qsbar.svg"
for size in 48 128 256; do
  install -Dm644 "assets/qsbar-$size.png" "$HOME/.local/share/icons/hicolor/${size}x${size}/apps/qsbar.png"
done
gtk-update-icon-cache -f -q -t "$HOME/.local/share/icons/hicolor" 2>/dev/null || true

plugins="$HOME/.config/DankMaterialShell/plugins"
mkdir -p "$plugins"
ln -sfn "$PWD/dms-plugin" "$plugins/quickShareBar"

systemctl --user daemon-reload
systemctl --user enable --now qsbar.service
systemctl --user restart qsbar.service
dms ipc call plugins reload quickShareBar >/dev/null 2>&1 || true
echo "qsbar installed. Add the 'Quick Share' widget to your bar in DMS settings if it is not there yet."
