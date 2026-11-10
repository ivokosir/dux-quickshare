#!/usr/bin/env bash
# Builds qsbar, installs the user service and the Dux Shell (dms) plugin.
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

# Dux Shell keeps its config in ~/.config/dms. Only an existing older setup
# (DankMaterialShell, no dms folder yet) still uses the old place.
conf="${XDG_CONFIG_HOME:-$HOME/.config}"
shell_conf="$conf/dms"
if [[ ! -d "$shell_conf" && -d "$conf/DankMaterialShell" ]]; then
  shell_conf="$conf/DankMaterialShell"
fi
plugins="$shell_conf/plugins"
mkdir -p "$plugins"
ln -sfn "$PWD/dms-plugin" "$plugins/quickShareBar"

systemctl --user daemon-reload
systemctl --user enable --now qsbar.service
systemctl --user restart qsbar.service
dms ipc call plugins reload quickShareBar >/dev/null 2>&1 || true
echo "qsbar installed. Add the 'Quick Share' widget to your bar in the shell's settings if it is not there yet."
