# NoSignal

**Software "unplug" for your displays.** Turn any connected monitor or TV off in software so it behaves as if the cable were pulled — the desktop shrinks, windows migrate, the cursor is fenced, and the display drops to standby on its own — then bring it back on demand with its exact layout restored.

> **Status: pre-release scaffold.** Nothing here is usable yet; the project is under active initial development. Watch the releases page.

## Why

Every OS *can* deactivate a display, but the experience is buried, inconsistent, and un-automatable. Nothing open-source and cross-platform exists that offers a daemon, profiles, GUI, tray, hotkeys, and scriptable automation for "keep this display off until I want it." The canonical use case: a TV permanently plugged into a desktop GPU, off 90% of the time, toggled on for movie or gaming sessions — and it *stays* off across reboots, re-logins, and the TV being power-cycled.

## Features (v1 targets)

- **Flip the switch from anywhere:** GUI window (optionally retreating to a tray icon), tray menu, global hotkeys, CLI (`nosignal off tv`), and scriptable IPC — on Linux the daemon's DBus API (`org.nosignal.Daemon1`, JSON payloads) is the automation hook, `busctl`/`gdbus`-friendly.
- **Profiles** storing full display layouts (on/off, mode, position, scale, rotation, primary), hot-switchable via hotkey, tray, or GUI.
- **Persistence:** a background daemon re-asserts "this display stays off" after reboots, re-logins, hotplug events, KVM switches, and monitor power-cycles.
- **Robust display identity:** displays are matched by EDID first (survives replugging into a different port), connector name as fallback, with explicit warnings on ambiguity — never a guess.
- **Audio follow-through:** if the disabled output was the HDMI/DP audio sink, the default sink fails over and is restored on re-enable.
- **Safety nets:** disabling the last active display (or a laptop's internal panel) always arms a timed auto-revert, like OS resolution changes.
- **DDC/CI instant sleep (opt-in):** tell capable monitors to power down immediately instead of waiting for their no-signal timer.

## Platform support

| Platform | Backend | Status |
|---|---|---|
| Linux / GNOME (Wayland & X11) | Mutter DisplayConfig DBus | CLI + daemon working (GUI in progress) |
| Linux / KDE Plasma | KScreen | planned (v1) |
| Linux / wlroots (Sway, Hyprland, …) | `zwlr_output_manager_v1` | planned (v1) |
| Linux / other X11 (XFCE, MATE, Cinnamon, …) | RandR | planned (v1) |
| Windows 10/11 | CCD API | planned (v1) |
| macOS | — | open for a contributor (see [CONTRIBUTING](CONTRIBUTING.md)) |

## Known physical-layer realities

These are properties of display hardware, not NoSignal bugs:

- **DisplayPort monitors** in deep sleep or hard-off can drop off the bus entirely (EDID unreadable), so re-enabling may fail until you wake the monitor with its power button. HDMI devices generally keep EDID readable while asleep, so re-enable is reliable.
- **DDC/CI** frequently breaks through docks, adapters, MST hubs, and KVMs — it's a per-path capability, probed per output, never assumed.
- **DP MST daisy chains** re-enumerate on topology changes and can shuffle outputs; EDID-first identity absorbs most of this.
- **TVs over HDMI** may take minutes to notice signal loss; instant TV standby needs CEC (future roadmap).

## Architecture (short version)

A per-session daemon (`nosignald`) owns all display logic, state, profiles, guards, and timers. The GUI (Tauri v2 + Svelte), tray, CLI (`nosignal`), and hotkeys are thin clients over IPC — DBus on Linux (which doubles as the automation hook), named pipes on Windows. Display-server specifics live behind a backend trait with implementations per compositor/OS. See [docs/SPEC.md](docs/SPEC.md) for the archived research spec.

## Roadmap (post-v1)

HTTP/MQTT endpoint (Home Assistant), HDMI-CEC / network TV control, GNOME Shell companion extension (fullscreen detection), window position save/restore, macOS backend, Flathub/PPA/AUR/winget packaging.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Contributions require signing the [CLA](CLA.md). The most-wanted areas: a macOS backend, and testing/fixes on desktop environments the maintainer can't test personally.

## License

[AGPL-3.0-only](LICENSE). Contributor License Agreement required for PRs; see [CONTRIBUTING.md](CONTRIBUTING.md) for the rationale.
