# Signifiage

Signage, stage playback and queue console for Linux — one GTK4/libadwaita + GStreamer
application that designs scenes, drives program/preview/teleprompter outputs, plays
text/image/video/capture sources, keeps a called name queue fed from spreadsheets,
runs a soundboard and timers, and also runs headless-ish on a signage PC with a LAN
web remote for phones and tablets.

Interface languages: English and Turkish.

## Features

- **Scenes and layers** — text, clock, timer, ETA, image, video, capture, queue,
  ticker and built-in test patterns on a fixed design canvas, hard cuts only.
- **Independent outputs** — program, preview and prompter windows on chosen
  monitors, fullscreen or windowed; rotation and fit/fill/stretch.
- **Playlists and schedules** — per-item durations, loop, player-default playlist,
  weekday/time windows evaluated by the player.
- **Queue** — import `.xlsx`, `.xls` or `.ods` with a mapping wizard, keep it
  refreshing every 2 s, call/recall/done/skip with undo; manual entry too.
- **Soundboard** — three pages of pads, F1–F12 hotkeys, loop, master mute.
- **Teleprompter** — scrolling script with speed, size, mirroring and its own output.
- **Capture and deck control** — cameras (v4l2), X11 windows (`ximagesrc`) and
  Wayland screens (xdg-desktop-portal); next/previous key injection through XTest
  or the RemoteDesktop portal.
- **Player mode** — `signifiage player` plays a project or a packaged
  `.signifiage` file on a signage PC, optionally with the web remote.
- **Web remote** — dependency-free HTML/JS dashboard over the LAN with a token.

## Prerequisites

Runtime: GTK 4.18+, libadwaita 1.6+, GStreamer 1.22+ with
`gstreamer1.0-gtk4`, `gstreamer1.0-plugins-base/good/bad`, `gstreamer1.0-libav`,
`gstreamer1.0-pipewire` (for portal capture), and `xdg-desktop-portal` plus a
backend such as `xdg-desktop-portal-gnome` for screen capture and key injection on
Wayland.

Build: Rust 1.92 or newer (edition 2024) and the development headers.

```bash
bash scripts/bootstrap_dev.sh   # one-time: apt dev headers + rustup
```

## Run from source

```bash
cargo run --release                       # last project or the welcome page
cargo run --release -- ~/Shows/Stage2026  # open a project directory
cargo run --release -- player ~/Shows/Stage2026 --windowed --remote
```

## Build the .deb

```bash
bash scripts/build_deb.sh
# build/signifiage_<version>_amd64.deb  (+ .sha256)
```

The package installs `/usr/bin/signifiage`, the desktop entry, the icon, the
AppStream metainfo and the docs. Assets and web UI are embedded in the binary.

## Player and publishing

```bash
signifiage publish ~/Shows/Stage2026 ~/out/Stage2026.signifiage
signifiage player ~/out/Stage2026.signifiage --remote --port 8720
```

`publish` writes a ZIP with `project.json` plus `assets/`. The player extracts it to
a temporary directory, plays the default playlist (or the schedule matching the
current weekday/time, re-evaluated every 30 s), and keeps queue progress in
`${XDG_STATE_HOME:-~/.local/state}/signifiage/player-<key>.json` so a restart keeps
the called name. The package itself is never modified.

## Web remote

Enable it in Settings → Remote (or pass `--remote` to the player). The dashboard
prints `http://<lan-ip>:<port>`; open it with `?token=<token>` once, the token is
stored in the browser. It is plain HTTP on the LAN — no TLS — so keep it on a
trusted network. Uploads are only accepted while the GUI is running.

## Capture backends

- **Camera** — `v4l2src`, no portal needed.
- **X11 window** — `ximagesrc` by window id; works on an X11 session or XWayland.
  Window lists are read from `$DISPLAY`.
- **Screen (portal)** — `xdg-desktop-portal` + PipeWire. The GNOME picker must be
  approved interactively; unattended players need someone at the machine after
  boot. Restore tokens are stored so re-picking is quick.

Key injection mirrors this: X11 uses XTest (focus is set for the captured window);
Wayland uses the RemoteDesktop portal and cannot change focus, so desktop deck
buttons wait 2 s for you to focus the presentation, while the web remote sends the
key immediately to whatever has focus.

## Monitors and the operator console

Outputs are placed with `fullscreen_on_monitor`. GTK4 cannot move the operator
window between monitors on Wayland; the Settings option "operator console display"
therefore means "fullscreen the main window on that monitor", and "Desktop default"
leaves placement to the window manager. Multiple outputs may share one monitor.

## Keyboard shortcuts

Unless a text field has focus: `Space` go/next, `Backspace` previous, `B` black,
`F` freeze, `M` mute, `S` soundboard dock, `F1`–`F12` pads, `Ctrl+S` save,
`Ctrl+Z`/`Ctrl+Shift+Z` undo/redo, `Ctrl+N` new, `Ctrl+O` open, `Ctrl+P` publish,
`Delete` remove selected layer, `Ctrl+D` duplicate layer, `Esc` close dock/menus.
On the design canvas: drag to move, corner handles to resize, arrows nudge 1 px
(`Shift` 10), `Shift` disables grid snapping.

## Development

```bash
cargo test --locked                 # model, queue, spreadsheet, timers, schedule,
                                    # controller, capture, deck, x11, web, packages
cargo fmt && cargo clippy --all-targets

# Offscreen proofs / smoke harnesses
cargo run --release --example render_scene -- --out /tmp/scene.png
cargo run --release --example gui_shot -- --project tests/fixtures/demo-project --page run --out /tmp/run.png
cargo run --release --example dialog_smoke -- tests/fixtures/demo-project
```

`render_scene` renders one scene to a PNG, `gui_shot` renders a whole window page
(design/run/queue/prompter/soundboard) into a PNG, and `dialog_smoke` constructs the
New Project, Timers, Schedules and About dialogs against a real project to catch
construction-time GTK errors without clicking through the UI.

Projects are plain directories: `project.json` plus `assets/`. Saves are atomic
(`project.json.tmp` → rename) with `project.json.bak` as the fallback. Scenes,
playlists and schedules share one 50-entry undo stack that never touches live
queue state; the queue has its own stack.

Bundle metadata: the desktop entry, the AppStream metainfo and the SVG icon live
in `data/`, the GTK stylesheet in `data/style.css`, and the web remote in `web/`
(all embedded into the binary).

## License

GPL-3.0-or-later. See `LICENSE`.
