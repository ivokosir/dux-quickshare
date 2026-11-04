# quickshare-bar

Receive files from Android **Quick Share** without a window. Everything happens
in your [DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell) bar.

- **One-click visibility:** right-click the bar icon to switch between visible and hidden.
  The popup also has *Hidden · 10 min · Visible*.
- **Copy, don't save:** a received photo goes straight to the clipboard as PNG,
  with EXIF rotation applied. Copied files are kept only in RAM (`$XDG_RUNTIME_DIR`)
  so you can copy them again from the bar. Nothing is written to your folders,
  and they disappear at logout or reboot.
- **Save when you want:** *Save* moves the file to your Downloads folder, either right
  away or later from the Recent list.
- **Notifications with actions:** *Copy / Save / Decline* on every request, with the PIN shown.
- **Auto-accept** for your own phone ("Always accept from …").
- Text, links and Wi-Fi passwords sent from the phone are copied too.
- If a phone tries to share while you are hidden (seen over Bluetooth), you get
  a "Become visible" prompt.

It replaces the [RQuickShare](https://github.com/Martichou/rquickshare) window. Its
protocol library (`rqs_lib`) is vendored and runs headless in a small daemon.

## How it works

```
phone ──Quick Share──▶ qsbar daemon (Rust, rqs_lib) ──unix socket──▶ DMS bar plugin (QML)
                              │
                              ├─ wl-copy      (clipboard)
                              └─ notify-send  (notifications with actions)
```

`qsbar watch` streams the state as JSON lines. Every other subcommand is a one-shot
command, so you can also script it or bind it to keys:

```
qsbar visible | hidden | toggle | temp 10
qsbar accept <id> [copy|save] | reject <id>
qsbar copy|save|open|reveal|remove <item>
qsbar trust "<device name>" | untrust "<device name>"
qsbar status
```

The plugin also registers IPC handlers, for example for niri keybinds:

```
dms ipc call quickshare open        # open the popup
dms ipc call quickshare visibility  # toggle visible/hidden
```

## Requirements

- Linux on Wayland, with [DankMaterialShell](https://github.com/AvengeMedia/DankMaterialShell) ≥ 1.5
- `wl-clipboard`, `libnotify` (`notify-send`), `systemd` (user services, `busctl`)
- Rust toolchain, `protobuf` (`protoc`) and `dbus` headers to build
- Optional: `imagemagick` to copy HEIC/AVIF photos; BlueZ for the "phone nearby" hint
- The phone and the PC must be on the same Wi-Fi/LAN

## Install

```sh
./install.sh
```

This builds `qsbar`, installs it to `~/.local/bin`, enables the `qsbar.service`
user unit, and links the plugin into `~/.config/DankMaterialShell/plugins/quickShareBar`.
After that, add the **Quick Share** widget to a bar in DMS settings.

If RQuickShare is installed, stop it and turn off its autostart. Two receivers on
one machine show up twice on the phone.

## Files

| What | Where |
| --- | --- |
| Copied (not saved) files | `$XDG_RUNTIME_DIR/qsbar/files` (RAM) |
| Transfers larger than 300 MB, while arriving | `~/.cache/qsbar/` (cleared on restart) |
| Saved files | your XDG Downloads folder |
| Settings and history | `~/.local/state/qsbar/state.json` |

## Testing without a phone

`examples/send.rs` is a tiny Quick Share sender built on the same library:

```sh
cargo run --release --example send -- 127.0.0.1:<port> photo.jpg
```

The port is logged by the daemon on start: `journalctl --user -u qsbar | grep TcpListener`.

## License

GPL-3.0-or-later. See [LICENSE](LICENSE). It includes `vendor/rqs_lib` from
[RQuickShare](https://github.com/Martichou/rquickshare) by Martichou (GPL-3.0); see
[vendor/rqs_lib/PATCHES.md](vendor/rqs_lib/PATCHES.md) for the local changes.
