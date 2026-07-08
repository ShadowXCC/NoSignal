# Contributing to NoSignal

Thanks for considering a contribution! This document covers project structure, workflow, and the areas where help is most wanted.

## Licensing & CLA

NoSignal is licensed [AGPL-3.0-only](LICENSE). All contributions require agreeing to the [Contributor License Agreement](CLA.md) — a bot will prompt you on your first pull request; signing is a single PR comment.

**Why a CLA?** It grants the maintainer the right to relicense contributions, which keeps a future sustainability path open (dual licensing / separately distributed commercial add-ons). The promise in return, written into the CLA itself: the AGPL core stays fully functional free software, and commercial features live in separate processes over the daemon's public IPC — never as withheld core functionality.

## Project structure

```
crates/
  nosignal-core/          Backend trait, topology types, EDID identity matching,
                          profile schema, guard logic. Platform-independent.
  nosignal-ipc/           IPC client/server plumbing (DBus on Linux, named pipes on Windows).
  nosignal-backend-*/     One crate per display backend (gnome, kde, wlroots, x11, win, mock).
  nosignal-ddc/           Opt-in DDC/CI monitor power control.
  nosignald/              The daemon. Owns ALL state and logic.
  nosignal-cli/           `nosignal` command-line client.
gui/                      Tauri v2 + Svelte GUI (thin client; excluded from the root
                          Cargo workspace so core builds don't need webkit2gtk).
packaging/                systemd user unit, udev rules, desktop files.
```

**The golden rule:** clients are dumb. The CLI, GUI, and tray contain zero display logic — they render daemon state and send daemon commands. All behavior lives in `nosignald` and the backend crates.

## Development setup

- Rust (stable, 1.85+), plus `cargo fmt` and `cargo clippy`.
- Node 20+ for the GUI.
- Linux GUI builds need the [Tauri v2 Linux prerequisites](https://tauri.app/start/prerequisites/) (webkit2gtk 4.1, gtk3, etc.). Not needed for daemon/CLI work.

```sh
cargo test --workspace          # core + daemon logic is fully testable headless (mock backend)
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
cd gui && npm ci && npm run check && npm run build
```

CI runs exactly these on Ubuntu and Windows for every PR.

## Adding a display backend

Implement the `DisplayBackend` trait from `nosignal-core` in a new `nosignal-backend-<name>` crate: `snapshot()` (current topology), `apply()` (whole desired layouts — never single-output deltas), `watch()` (topology change events), `capabilities()`. Study `nosignal-backend-mock` for the contract and `nosignal-backend-gnome` for a real example. Wire detection into the daemon's backend auto-detection.

### Most-wanted: macOS backend

macOS support is explicitly reserved for a contributor with Apple hardware. Prior art and guidance:

- **BetterDisplay** (closed source) proves the capability exists; **DisableMonitor** (abandoned, open source) shows the old private-API approach.
- The relevant surface is private CoreGraphics APIs (`CGSConfigureDisplayEnabled` and friends) on Apple Silicon / recent macOS.
- Expect the same identity rules (EDID-first via `CGDisplayCreateUUIDFromDisplayID` analogues) and the same whole-layout apply model.
- IPC on macOS should follow the Unix path (a domain socket is fine); the daemon runs as a LaunchAgent.

Open an issue before starting so the trait contract can be firmed up together.

### Also wanted: testing on more desktop environments

The maintainer can personally test GNOME (Wayland) and Windows. KDE, wlroots compositors (Sway, Hyprland), and X11 desktops (XFCE, MATE, Cinnamon) are implemented from documentation and VM testing — real-hardware reports and fixes are hugely valuable. Please include `nosignal status --json` output and daemon logs in bug reports.

## Pull request checklist

1. `cargo fmt`, `clippy -D warnings`, and `cargo test --workspace` pass locally.
2. New daemon/core behavior comes with tests against the mock backend.
3. User-facing features update the README.
4. Physical-hardware quirks you discover (dock, KVM, MST, deep-sleep behaviors) get documented — these are load-bearing knowledge for this project.
