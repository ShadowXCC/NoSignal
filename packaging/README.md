# packaging/

System integration files bundled into the Linux packages by `release.yml`.

| File | Purpose | Lands in package at |
|---|---|---|
| `nosignal-daemon.service` | systemd user unit for `nosignald` | `/usr/lib/systemd/user/` |
| `99-nosignal-i2c.rules` | udev rule for the opt-in DDC/CI feature | `/usr/lib/udev/rules.d/` |
| `nosignal.desktop` | desktop entry for the GUI (reference; Tauri generates its own) | `/usr/share/applications/` |
| `io.github.shadowxcc.NoSignal.Daemon1.service` | DBus activation: any client call auto-starts the daemon | `/usr/share/dbus-1/services/` |

Post-install behavior: packages print (but never force) `systemctl --user enable --now nosignal-daemon`.
