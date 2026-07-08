# NoSignal — Project Handoff & Build Specification

> **One-liner:** Software "unplug" for displays. Turn any connected monitor or TV off in software so it behaves as if the cable were pulled — the desktop shrinks, the mouse can't reach it, and the display goes to sleep — then bring it back on demand.

**Status of this document:** Complete v1 specification agreed with the project owner. All major decisions below are settled; see §17 for the small set of questions intentionally deferred to implementation time.

---

## 1. Decision ledger (settled — do not relitigate)

| Decision | Choice |
|---|---|
| Name | **NoSignal** (one word, capital N/S in prose; `nosignal` in code) |
| License | **AGPLv3** + Contributor License Agreement (CLA) to preserve future dual-licensing/Pro-tier options |
| Language | **Rust** (Cargo workspace) |
| GUI | **Tauri v2 + Svelte** (TypeScript) |
| Process model | **Daemon + thin clients** (CLI, GUI, tray, hotkeys all talk to the daemon over IPC) |
| IPC (Linux) | **DBus session bus**, well-known name `dev.nosignal.Daemon1` (adjust to owner's registered domain; fallback convention `io.github.<owner>.NoSignal`) |
| v1 platform | **Linux / GNOME on Wayland** (owner runs Ubuntu 26.04 LTS, GNOME 50, Wayland-only) |
| v1.x platform | **Windows 10/11** via CCD API |
| Explicitly future | macOS backend, KDE/wlroots/X11 backends, DDC/CI, HTTP/MQTT, CEC — see §15 |
| Packaging | GitHub Actions → `.deb`, `.rpm`, AppImage (Linux) + `.msi`, NSIS `.exe` (Windows) |
| Primary owner use case | A TV permanently plugged into a desktop GPU, disabled ~90% of the time, toggled on for movie/gaming sessions |

---

## 2. Problem statement & prior art

Every OS can technically deactivate a display, but the experience is buried, inconsistent, and un-automatable:

- **Windows** hides "Disconnect this display" in Settings; NirSoft MultiMonitorTool covers scripting but is closed-source freeware with documented Windows 11 24H2 reliability problems.
- **macOS** is owned by BetterDisplay (closed source, freemium) using private APIs; the open-source predecessor (DisableMonitor) is abandoned.
- **Linux** has the capability natively (`xrandr --off`, `kscreen-doctor`, Mutter DBus, `wlr-randr`) but no polished, compositor-aware tool with a daemon, profiles, GUI, tray, and hotkeys.
- **Nothing cross-platform and open-source exists.** That is the niche.

Adjacent prior art worth reading before coding:
- `gnome-randr` / `gnome-monitor-config` — working consumers of the Mutter DisplayConfig DBus API.
- `haimgel/display-switch` (Rust) — cross-platform monitor plumbing patterns (it does DDC input switching, not disconnect).
- NirSoft MultiMonitorTool docs — a catalog of Windows CCD failure modes learned the hard way.

---

## 3. Core semantics: what "off" means

**"Off" = output deactivation**, not hot-plug emulation:

1. NoSignal asks the display server / OS to remove the target display from the desktop topology.
2. The GPU stops driving the connector. The desktop re-tiles onto remaining displays; the OS migrates windows automatically; the cursor is fenced.
3. The display sees "no signal" and drops to standby **on its own timer** (seconds for most monitors, up to a few minutes for TVs). NoSignal does not wait for or verify this.
4. **"On"** re-adds the display to the topology with its remembered mode, position, scale, and rotation. The returning signal wakes the display.

Non-goals (v1): making the kernel/OS believe the cable was physically pulled (HPD emulation); forcing the panel dark faster than its own no-signal timer (that is the future DDC/CI feature, §15).

**Known physical-layer realities to encode as expectations, not bugs:**
- HDMI devices keep EDID readable while asleep → re-enable is reliable. **DP monitors in deep sleep or hard-off can drop off the bus entirely** → re-enable may fail until the user wakes the monitor; surface a clear error, retry briefly, never crash.
- Protocol *versions* (DP 1.2 vs 1.4, HDMI 2.0 vs 2.1) are irrelevant at this layer. Connector *type* only matters via the sleep/EDID behaviors above.

---

## 4. Architecture

```
┌────────────┐  ┌────────────┐  ┌────────────────────┐
│ nosignal   │  │ tray icon  │  │ nosignal-gui       │
│ (CLI)      │  │ (in GUI    │  │ (Tauri v2 + Svelte)│
└─────┬──────┘  │  process)  │  └─────────┬──────────┘
      │         └─────┬──────┘            │
      └───────────────┴───────────────────┘
                      │  DBus session bus (Linux) / named pipe (Windows)
              ┌───────▼────────┐
              │   nosignald    │   owns ALL logic: state, profiles,
              │   (daemon)     │   guards, timers, persistence, audio
              └───────┬────────┘
                      │  Backend trait
      ┌───────────────┼───────────────────────────┐
┌─────▼─────┐  ┌──────▼──────┐  ┌─────────────────▼──┐
│ GNOME     │  │ Windows CCD │  │ (future: wlroots,  │
│ (Mutter   │  │ (v1.x)      │  │  KDE, X11, macOS)  │
│  DBus) v1 │  └─────────────┘  └────────────────────┘
└───────────┘
```

**Golden rule:** clients are dumb. The CLI, GUI, and tray contain zero display logic; they render daemon state and send daemon commands. This is what makes future backends, a native-GTK contributor GUI, and the eventual HTTP/MQTT surface cheap.

### Cargo workspace layout

```
nosignal/
├── Cargo.toml                 # workspace
├── crates/
│   ├── nosignal-core/         # Backend trait, types, identity matching, profile schema
│   ├── nosignal-backend-gnome/# Mutter DisplayConfig implementation
│   ├── nosignal-backend-win/  # Windows CCD implementation (v1.x; scaffold from day 1)
│   ├── nosignal-backend-mock/ # In-memory backend for tests
│   ├── nosignald/             # Daemon binary: DBus service, timers, persistence, audio
│   └── nosignal-cli/          # `nosignal` binary (clap)
├── gui/                       # Tauri v2 app (Svelte + TS frontend, thin Rust shell)
├── packaging/                 # systemd user unit, udev rules (future DDC), desktop files
├── .github/workflows/         # ci.yml (PR), release.yml (tags)
├── CLA.md + cla-assistant config
├── CONTRIBUTING.md            # includes macOS backend guidance (§15)
└── LICENSE                    # AGPL-3.0
```

---

## 5. Core abstractions (`nosignal-core`)

### 5.1 Backend trait (illustrative sketch — refine at implementation)

```rust
#[async_trait]
pub trait DisplayBackend: Send + Sync {
    fn name(&self) -> &'static str;                    // "gnome", "windows-ccd", "mock"
    fn capabilities(&self) -> Capabilities;            // persistence, per-output audio hint, ...
    async fn snapshot(&self) -> Result<Topology>;      // all outputs + current layout + config serial
    async fn apply(&self, plan: &LayoutPlan, mode: ApplyMode) -> Result<()>;
    async fn watch(&self) -> Result<BoxStream<'static, TopologyEvent>>; // hotplug/config-change events
}

pub enum ApplyMode { Verify, Temporary, Persistent }   // maps 1:1 onto Mutter's method flag
```

`LayoutPlan` is a full desired topology (every output: enabled/disabled, mode, position, scale, rotation, primary). Backends apply whole layouts, never single-output deltas — this matches how Mutter and CCD actually work and sidesteps ordering bugs.

### 5.2 Output identity — the EDID-first rule

```rust
pub struct OutputIdentity {
    pub edid: Option<EdidId>,   // { vendor: String, product: String, serial: String }
    pub connector: String,      // "HDMI-A-1", "DP-3", "eDP-1", "\\\\.\\DISPLAY1"
}
```

Matching priority when resolving a profile entry or CLI target to a live output:
1. **EDID triple** (vendor+product+serial) — survives replugging into a different port.
2. **Connector name** — fallback when EDID is absent/garbage.
3. **Ambiguity** (two live outputs match, e.g. identical monitors reporting serial `0`) → hard error listing candidates by connector; never guess. Profiles store *both* identifiers plus a user-facing alias ("TV", "left").

CLI/GUI accept any of: alias, connector, or EDID substring.

### 5.3 Profile schema (v1, versioned from day one)

Stored at `$XDG_CONFIG_HOME/nosignal/` (daemon-owned; TOML or JSON — implementer's choice, but include `schema_version: 1`).

```toml
schema_version = 1

[[profiles]]
name = "desk"                   # hotswitchable by name
hotkey = "<Super>F9"            # optional; see §10.3
[[profiles.outputs]]
alias = "TV"
edid = { vendor = "SAM", product = "0x1234", serial = "..." }
connector = "HDMI-A-1"
enabled = false
[[profiles.outputs]]
alias = "main"
edid = { ... }
connector = "DP-1"
enabled = true
mode = "3840x2160@144"          # optional pins; omitted = current/preferred
position = [0, 0]
primary = true

[profiles.audio]                # optional
preferred_sink = "alsa_output.pci-...analog-stereo"
```

**Semantics:** a profile describes desired states for the outputs it names and is silent about others. The daemon tracks one **active profile**; on any `TopologyEvent` (hotplug, TV power-cycled, resume) it re-asserts the active profile — this is the persistence engine (§8.3).

---

## 6. GNOME backend (v1 primary) — `nosignal-backend-gnome`

Target: GNOME 46+ on Wayland (owner: GNOME 50 / Ubuntu 26.04, which is Wayland-only). Use the `zbus` crate.

**Interface:** `org.gnome.Mutter.DisplayConfig` at object path `/org/gnome/Mutter/DisplayConfig` on the session bus. Nominally private but stable for many years; pin behavior with integration tests and crib call shapes from `gnome-randr` / `gnome-monitor-config` rather than guessing.

Key mechanics the implementation must respect:

1. **`GetCurrentState()`** returns `(serial, monitors, logical_monitors, properties)`. Each monitor carries its connector plus EDID vendor/product/serial (feeds §5.2 directly) and available modes.
2. **`ApplyMonitorsConfig(serial, method, logical_monitors, properties)`**:
   - `serial` must be the one from the latest `GetCurrentState`; stale serial ⇒ error. Always re-read immediately before apply; retry once on serial races.
   - `method`: `0 = verify`, `1 = temporary` (reverts on session restart), `2 = persistent` (Mutter writes `~/.config/monitors.xml`).
   - **Disable = omit** the monitor from `logical_monitors`. There is no "disabled: true" flag.
3. **Layout validity is on us.** After removing a logical monitor, re-normalize the remaining layout: exactly one primary, no overlaps, translate so the bounding box origin is (0,0). Store the removed monitor's full logical config (mode, position, scale, transform, primary) inside the daemon so re-enable restores it exactly.
4. **`MonitorsChanged` signal** → emit `TopologyEvent`. This fires on hotplug, resume, and external config changes (user opening GNOME Settings). Debounce (~500ms) before re-asserting profiles to avoid fighting Mutter mid-negotiation.
5. **Persistence interplay:** GNOME keys `monitors.xml` entries to the *set of connected monitors*. Applying the disabled layout with `method=2` makes GNOME itself keep the TV off across reboots/logins for that hardware combo — the daemon's re-assert loop is the backstop, not the only mechanism. Auto-revert flows use `method=1` until confirmed (§8.2).
6. **Fractional scaling:** logical monitor scale values must be ones Mutter offers for the mode; reuse the value from `GetCurrentState`, never compute.

**GNOME environment notes (affect clients, not backend):**
- Tray icons require the AppIndicator/SNI extension — enabled by default on Ubuntu, absent on stock GNOME. The tray is optional sugar; GUI + CLI must be fully sufficient without it.
- No global-hotkey API for regular Wayland apps → §10.3.
- Fullscreen detection is not possible from an external process on GNOME Wayland → §8.5.

## 7. Windows backend (v1.x) — `nosignal-backend-win`

Scaffold the crate and keep it compiling in CI from day 1 (`cargo check --target x86_64-pc-windows-msvc` or a Windows runner); implement in milestone M4.

- API: **CCD** via the `windows` crate — `QueryDisplayConfig` (snapshot; `DISPLAYCONFIG_TARGET_DEVICE_NAME` supplies EDID-ish identity) and `SetDisplayConfig` (apply). Disable = deactivate the target's path; apply with `SDC_APPLY | SDC_USE_SUPPLIED_DISPLAY_CONFIG | SDC_ALLOW_CHANGES | SDC_SAVE_TO_DATABASE` (the last flag = Windows-native persistence; map `ApplyMode` accordingly).
- Known terrain from MultiMonitorTool's changelog — treat as acceptance tests: Windows 11 24H2 can wedge until a display-settings poke (detect failure, surface actionable error); complex topologies may need config applied more than once (bounded retry, ~3 attempts); re-enabled monitors may come back at the wrong position (we restore position ourselves from stored state, same as GNOME).
- Fullscreen detection: `SHQueryUserNotificationState` (`QUNS_RUNNING_D3D_FULL_SCREEN` / `QUNS_BUSY`) — Windows gets real warn-and-defer in v1.x even though GNOME can't (§8.5).
- IPC: named pipe or local TCP behind the same client-facing interface; hotkeys/tray via Tauri plugins (both work normally on Windows).

---

## 8. Daemon spec — `nosignald`

Single-instance per session. On Linux: systemd **user** unit (`packaging/nosignal-daemon.service`, `WantedBy=graphical-session.target`), shipped in the .deb/.rpm; also DBus-activatable so any client can autostart it. Tokio runtime; `tracing` for logs.

### 8.1 DBus API surface (v1)

`dev.nosignal.Daemon1` — this IS the automation hook for v1 (`busctl`/`gdbus`-scriptable):

- `ListOutputs() → a(...)`: identity, alias, enabled, live/offline, current mode, position, primary, audio-sink association if known.
- `SetOutputEnabled(target: s, enabled: b, opts: a{sv}) → (job: o)`: `opts` includes `force`, `no_revert_timer`, `revert_secs`.
- `ConfirmPendingChange(job: o)` / `RevertPendingChange(job: o)`.
- `ListProfiles() → as` / `ApplyProfile(name: s, opts: a{sv})` / `SaveProfile(name: s)` (capture current state) / `DeleteProfile(name: s)`.
- `GetStatus() → a{sv}`: active profile, pending jobs, backend name, version.
- Signals: `OutputsChanged`, `PendingChange(job, seconds_remaining)`, `ProfileApplied(name)`.

### 8.2 Auto-revert engine (settled behavior)

Every risky apply follows GNOME's own resolution-change pattern:
1. Apply with `ApplyMode::Temporary`; create a pending job with a **20s default** countdown.
2. GUI shows a "Keep changes?" dialog; CLI prints instructions and (when interactive) prompts; the `PendingChange` signal drives both.
3. Confirm → re-apply `Persistent` (when the flow calls for persistence). Timeout or revert → restore prior layout.
4. **Risky is defined as:** disabling the last active display (allowed, per owner decision, but *always* with a mandatory revert timer — `force` cannot disable the timer for this case), disabling the internal laptop panel when it is the only display, or any apply the client flags. Routine toggles with ≥1 display remaining may skip the timer via `no_revert_timer` (CLI default for scripting: skip; GUI default: use timer).

### 8.3 Persistence / re-assert engine

- Daemon holds `active_profile` (persisted across restarts in state dir `$XDG_STATE_HOME/nosignal/`).
- On `TopologyEvent` (debounced): re-resolve the active profile against live outputs; if drift is detected (e.g., TV power-cycled and Mutter re-enabled it), re-apply. Log every re-assert.
- **Loop guard:** if re-assert triggers events causing >N applies (default 5) within 30s, stop, mark the profile "suspended," notify clients. Never fight the compositor indefinitely.
- Also re-assert on daemon startup (covers "GNOME reconnected everything at login," the exact failure BetterDisplay had to patch on macOS).

### 8.4 Audio module (Linux v1)

Owner decision: yes, handle audio.
- Before disabling an output, record the current default sink. If the default sink is the HDMI/DP audio device of that output, PipeWire will fail over on its own — but on re-enable, restore the remembered sink if the user hasn't changed it meanwhile.
- Implementation: shell out to `wpctl` for v1 (simple, robust) behind an `AudioController` trait; native `libpulse`/pipewire bindings and Windows `IPolicyConfig` are drop-in upgrades later.
- Associating an output with its sink: match PipeWire device/node names against connector/EDID heuristically; when ambiguous, expose manual association in the profile (`preferred_sink`).

### 8.5 Fullscreen warn-and-defer

- **GNOME Wayland (v1): detection is impossible from an external process** — do not fake it. v1 behavior: the auto-revert flow *is* the safety net; document the limitation.
- Design the daemon with a `FullscreenOracle` trait now: Windows impl in M4 (§7); GNOME impl arrives with the future companion Shell extension (§15). When an oracle reports fullscreen on an affected output, clients must confirm before apply ("Fullscreen app detected on TV — disable anyway?").


---

## 9. CLI spec — `nosignal`

`clap`-based, thin DBus client (auto-starts the daemon via DBus activation). Exit codes: 0 ok, 1 error, 2 ambiguous target, 3 guarded action refused.

```
nosignal list [--json]                      # outputs: alias, connector, EDID, state
nosignal off <target> [--force] [--no-timer] [--timer <secs>]
nosignal on  <target>
nosignal toggle <target>
nosignal status [--json]
nosignal profile list|apply <name>|save <name>|delete <name>
nosignal confirm | revert                   # resolve a pending change
nosignal hotkeys install                    # registers GNOME custom keybindings (§10.3 fallback)
nosignal daemon start|stop|status|logs
```

`<target>` = alias | connector | EDID substring (§5.2 resolution rules). `--json` everywhere for scripting — this plus raw DBus is the entire v1 automation story, by design.

---

## 10. GUI spec — Tauri v2 + Svelte

The GUI is a thin client: Svelte renders daemon state; the Tauri Rust shell proxies DBus calls/signals to the frontend via Tauri commands/events. **No display logic in the GUI process.**

### 10.1 Views (owner wants the full window, not a minimal toggle list)

1. **Displays** (home): spatial layout canvas mirroring the physical arrangement; disabled outputs rendered ghosted in their remembered position; click to toggle (with pending-change countdown UI); per-output detail panel (identity, mode, connector, audio sink).
2. **Profiles:** list, apply, save-current-as, delete; per-profile hotkey assignment; indicator for the active profile and a "drift" badge when live state ≠ profile.
3. **Settings:** revert-timer duration, tray on/off + close-to-tray, autostart (daemon unit + GUI), hotkey backend status (portal vs gsettings), guards config, log viewer.
4. **First-run:** detect outputs, prompt for aliases ("Which one is the TV?"), offer to create the user's first two profiles (e.g., `desk` / `movie`), offer autostart.

### 10.2 Tray

Tauri tray-icon plugin (SNI on Linux). Menu: per-output toggles, profile quick-switch, open GUI, quit. Detect environments without SNI support and degrade silently (GUI window remains the primary surface).

### 10.3 Hotkeys (Linux reality)

Tauri's global-shortcut plugin is Windows-only for our purposes. On GNOME:
1. **Preferred:** XDG `GlobalShortcuts` portal (`ashpd` crate) — available on recent GNOME including 26.04's GNOME 50; user grants via portal dialog.
2. **Fallback (always works):** `nosignal hotkeys install` writes GNOME custom keybindings (`org.gnome.settings-daemon.plugins.media-keys` via gsettings) that invoke the CLI. The GUI's hotkey settings page drives this and shows which mechanism is active.

### 10.4 Frontend notes

- Svelte 5 + TypeScript + Vite (Tauri default template). Keep dependencies light; no heavy UI kit required — the layout canvas is bespoke SVG/canvas anyway.
- All state flows through a single store fed by daemon events; optimistic UI only for toggle-click → pending state.
- Respect `prefers-color-scheme`; the app will sit next to GNOME dark mode.

---

## 11. Edge-case register (required behaviors)

| # | Case | Required behavior |
|---|---|---|
| E1 | Disable last active display | Allowed; mandatory 20s auto-revert, cannot be forced off (§8.2) |
| E2 | DP monitor in deep sleep won't re-enable (off the bus) | Bounded retries w/ backoff (~10s); then actionable error: "Wake the monitor (power button) and retry" |
| E3 | TV power-cycled while profile says off | `MonitorsChanged` → debounce → re-assert (§8.3) |
| E4 | GNOME Settings / another tool changes layout | Detect drift; do NOT auto-fight — mark profile drifted, notify; re-assert only on explicit apply or hotplug event |
| E5 | Re-assert loop (compositor fights back) | Loop guard: suspend profile after 5 applies/30s, notify (§8.3) |
| E6 | Identical monitors, EDID serial 0/duplicate | Ambiguity error listing connectors; require connector-level targeting; store alias→connector pin in profile |
| E7 | Monitor replugged into different port | EDID match wins; update stored connector; log the migration |
| E8 | Laptop lid close interplay | Do not override GNOME lid handling; if internal panel is profile-disabled and lid opens, re-assert applies profile (panel stays off) — document loudly; `nosignal on eDP-1` always available via hotkey/SSH |
| E9 | Suspend/resume | Treat resume as `TopologyEvent`; re-snapshot before any apply (serials invalid after resume) |
| E10 | HDMI/DP audio sink vanishes on disable | Expected; restore remembered sink on re-enable (§8.4) |
| E11 | Fullscreen app on target output | Windows: oracle + confirm. GNOME v1: revert-timer safety net only (§8.5) |
| E12 | Daemon crash/restart | State dir restores active profile + pending jobs; pending jobs older than timer ⇒ revert on startup |
| E13 | Windows 24H2 wedge / multi-apply quirks | Bounded retries; actionable error (§7) |
| E14 | MST daisy chains re-enumerate | EDID-first identity absorbs most of it; integration-test if hardware available; otherwise document as best-effort |
| E15 | Stale Mutter config serial | Re-read `GetCurrentState`, retry once (§6) |
| E16 | Two GPUs / mixed drivers | No special handling — backends operate at compositor/OS level; just don't assume one GPU in identity code |

---

## 12. CI/CD & packaging

### `ci.yml` (every PR/push)
- Matrix: `ubuntu-24.04`, `windows-latest`.
- `cargo fmt --check`, `cargo clippy --workspace -D warnings`, `cargo test --workspace` (mock backend makes core+daemon logic fully testable headless).
- Frontend: `npm ci && npm run check && npm run build` in `gui/`.
- Linux system deps for Tauri: `libwebkit2gtk-4.1-dev`, `libgtk-3-dev`, etc. (see Tauri v2 Linux prerequisites).

### `release.yml` (on tag `v*`)
- **tauri-action** on `ubuntu-24.04` → `.deb`, `.rpm`, AppImage; on `windows-latest` → `.msi` (WiX) + NSIS `.exe`.
- Build Linux packages on the **oldest supported Ubuntu runner** (glibc floor: 24.04-built binaries run on 26.04, not vice versa).
- **Sidecars:** bundle `nosignald` + `nosignal` as Tauri `externalBin` so one installer ships the whole suite. Build them first in the workflow, then let tauri-action package.
- `.deb`/`.rpm` extras: install the systemd user unit; postinst prints (does not force) `systemctl --user enable --now nosignal-daemon`; `.desktop` file + icon.
- Artifacts attach to a **draft** GitHub Release; owner publishes manually.
- Known deferred cost: **Windows code signing** (unsigned = SmartScreen warning). Documented in README; Azure Trusted Signing when the project warrants it. Post-v1 channels: PPA (separate Launchpad pipeline — do not entangle), Flathub (sandbox needs `--talk-name` for Mutter DBus; future i2c is harder), AUR, winget.

---

## 13. Licensing & governance setup (M0 tasks)

1. `LICENSE` = AGPL-3.0-only. SPDX headers via CI lint (optional but cheap).
2. **CLA before the first external PR:** cla-assistant (or cla-assistant-lite GitHub Action) with a grant broad enough to permit relicensing/dual-licensing by the owner. Rationale documented in CONTRIBUTING.md: keeps a future Pro tier possible while the project stays genuinely AGPL.
3. Open-core boundary principle (for the future, write it down now): the AGPL core stays fully functional; any commercial features ship as **separate processes/plugins over the daemon IPC**, never as core patches.

---

## 14. Milestones

- **M0 — Scaffold (small):** workspace, crates stubs (incl. `nosignal-backend-win` compiling), CI PR pipeline green on both OSes, LICENSE, CLA bot, CONTRIBUTING, this spec committed as `docs/SPEC.md`.
- **M1 — Core + GNOME + CLI:** `nosignal-core` types/trait, mock backend + tests, GNOME backend (snapshot/apply/watch), CLI `list/off/on/toggle/status` with revert flow. *Definition of done: owner toggles the TV from a terminal on Ubuntu 26.04, TV sleeps, `on` restores exact layout, survives a TV power-cycle via manual re-run.*
- **M2 — Daemon:** DBus service + activation, systemd unit, profiles CRUD + apply, active-profile re-assert engine + loop guard, audio module, state persistence, `nosignal daemon` subcommands. *DoD: TV stays off across reboot, re-login, and TV power-cycle with zero manual action.*
- **M3 — GUI:** Tauri app with all four views, tray, hotkeys (portal + gsettings fallback), first-run flow, autostart. *DoD: owner's household can use it without the terminal.*
- **M4 — Windows:** CCD backend, named-pipe IPC parity, fullscreen oracle, MSI/NSIS with sidecars + autostart. *DoD: feature parity minus audio (stretch) on a Win11 test box.*
- **M5 — v1.0:** docs site/README polish, edge-case register verified item-by-item, release pipeline exercised end-to-end, tagged release.

## 15. Future roadmap (explicitly NOT v1 — do not scope-creep)

| Item | Notes captured during design |
|---|---|
| DDC/CI power control (VCP 0xD6) | Instant panel standby for monitors. Linux: `ddc-hi` crate, i2c-dev + udev rule (ship rule in `packaging/` early). Windows: `SetVCPFeature`. Capability-probed, per-output opt-in. TVs generally don't support DDC/CI |
| HTTP/MQTT listener | Home Assistant integration; separate process/plugin over daemon IPC (also the open-core pattern proof) |
| CEC + network TV control | Instant TV off: Pulse-Eight USB-CEC or webOS/Tizen network APIs; plugin architecture |
| GNOME Shell companion extension | Unlocks real fullscreen detection (E11) and could unlock window-position restore |
| KDE (kscreen), wlroots (zwlr_output_manager_v1), X11 (RandR) backends | Trait impls; X11 relevant for non-GNOME distros only — owner's Ubuntu 26.04 is Wayland-only |
| macOS backend | Private CoreGraphics APIs, Apple Silicon/Ventura+; document in CONTRIBUTING for a future contributor; BetterDisplay is prior art |
| Window position save/restore | Windows: feasible (Win32). GNOME: needs the Shell extension. Deliberately cut from v1 |
| Windows code signing; PPA/Flathub/AUR/winget | §12 |
| Automation rules (idle timers, per-app) | Post-HTTP/MQTT |

## 16. Identifiers reference

| Thing | Value |
|---|---|
| Binaries | `nosignal` (CLI), `nosignald` (daemon), `nosignal-gui` |
| DBus name / path | `dev.nosignal.Daemon1` / `/dev/nosignal/Daemon1` (confirm against registered domain at M0) |
| Config / state | `$XDG_CONFIG_HOME/nosignal/` / `$XDG_STATE_HOME/nosignal/` |
| systemd user unit | `nosignal-daemon.service` |
| App ID (.desktop / Tauri) | `dev.nosignal.NoSignal` |
| Key crates | `zbus`, `tokio`, `clap`, `serde`, `tracing`, `async-trait`, `ashpd` (portal), `windows` (M4), `tauri` v2 (+ tray-icon, autostart, single-instance, updater plugins) |

## 17. Deferred to implementation time (the ONLY open questions)

1. TOML vs JSON for profile storage (schema in §5.3 is format-agnostic; pick when wiring serde).
2. Exact DBus signatures (§8.1 shapes are binding in spirit; finalize with zbus idioms).
3. Whether M4 includes Windows audio restore or defers it (stretch goal).
4. GUI layout-canvas interaction details (drag-to-arrange is NOT required for v1 — GNOME Settings owns arrangement; NoSignal owns on/off).

## 18. Verification checklist for the owner's own setup

After M2, on the Ubuntu 26.04 + TV rig: `nosignal off tv` → TV shows NO SIGNAL then sleeps; cursor fenced; windows migrated. Reboot → TV still off. Power-cycle the TV at the wall → back to sleep within seconds (watch the daemon log re-assert). Movie night: hotkey → `movie` profile → TV on at remembered mode/position, audio sink restored. `nosignal on tv` after the TV was hard-off at the wall → actionable retry message, not a crash (E2).