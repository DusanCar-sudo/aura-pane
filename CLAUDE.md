# CLAUDE.md — aura-pane

Right-side Aura pane for **Aura OS** (Arch Linux + sway, Wayland). Tauri 2 app whose webview lives on a **wlr-layer-shell** surface anchored to the right edge. Linux/sway only — no Windows/macOS targets.

Owner: Dule. Style: concise, direct. Always deliver complete files, never partial snippets.

## Commands

```bash
npm install                 # once
npm run dev                 # dev build + run
npm run build               # release → src-tauri/target/release/aura-pane
cd src-tauri && cargo check # fast Rust compile check
node --check src/main.js    # JS syntax check
install -Dm755 src-tauri/target/release/aura-pane ~/.local/bin/aura-pane
pkill aura-pane; aura-pane  # restart with logs in the terminal
```

System deps: `webkit2gtk-4.1 gtk3 gtk-layer-shell nodejs npm rustup base-devel`.

## Layout

```
src/                    frontend — vanilla HTML/CSS/JS, no bundler
  index.html            pane markup (clock, Recent, Aura box, pin/expand buttons, edge strip)
  styles.css            tokens on :root, rainbow ring, auto-hide slide
  main.js               state, render(), show/hide logic, Tauri invoke/listen
src-tauri/
  src/main.rs           layer shell, keyboard focus, sway IPC, commands
  tauri.conf.json       window "main" (hidden, transparent, undecorated), withGlobalTauri
  capabilities/default.json
  icons/icon.png
sway/aura-pane.conf     exec + Super+I binding (include from sway config)
```

## Architecture

### Layer shell (main.rs `setup_layer_shell`)
- Tauri's GTK window is already realized, so gtk-layer-shell can't be initialized on it. We create a **new `gtk::ApplicationWindow`**, `init_layer_shell()` it, **reparent the webview** children into it, then hide the original.
- GTK objects are `!Send` → stored in `thread_local!` (`PANE`, `WEBVIEW`, `VIS`). Touch them only on the main thread via `app.run_on_main_thread`.
- Name collisions: use `set_layer_shell_margin` (not `set_margin`, which is GTK's widget margin) and `WidgetExt::screen(&w)` explicitly.

### Keyboard focus
- Default mode `KeyboardMode::OnDemand` (clicking a tile takes the keyboard back).
- `grab_keyboard()`: switch to `Exclusive` + `present()` + `grab_focus()` on the WebKitWebView, then after `GRAB_MS` (250ms) revert to `OnDemand`. This was needed because sway gave the surface focus but GTK had no focused widget → typing did nothing.
- `find_webview()` walks the widget tree for a type name containing `WebKitWebView`.
- `connect_focus_in_event` re-focuses the webview whenever sway hands the surface the keyboard.
- Esc → `release_focus` → `swaymsg [con_id=N] focus` on the most recent window.

### Auto-hide (`VIS: (auto_hide, shown)`, `apply_layout()`)
- **Docked**: width 360, margins 10, `auto_exclusive_zone_enable()` → tiles reflow beside it.
- **Auto, hidden**: width 4 (`STRIP_WIDTH`), margins 0, exclusive zone 4 → only the rainbow edge strip.
- **Auto, shown**: width 360, exclusive zone stays 4 → pane overlays tiles, no reflow.
- Resizing via `set_size_request` + `resize`. The CSS slide (`--slide: 180ms`) runs inside the webview; on hide, JS removes `.shown`, waits `SLIDE_MS`, then calls `set_shown(false)` to shrink the surface.
- Mode persisted in `localStorage["aura.autohide"]` (default auto). JS calls `set_mode` on boot.

### Recent windows
- `seed_recent`: `swaymsg -r -t get_tree`, focused window first.
- `watch_sway`: long-running `swaymsg -r -m -t subscribe '["window"]'`; `focus` moves to front, `close` removes, `title` updates. Emits `aura://recent`.

### Hotkey
- `Super+I` → `aura-pane --focus`. `tauri-plugin-single-instance` forwards argv to the running instance → `focus_input()` shows the pane, grabs keyboard, emits `aura://focus`.
- **Taken keys on Dule's setup — don't use:** `Super+Space` (settings panel), `Super+A` (Aura terminal).

### Engine
- `ask(prompt)`: POST `{"message": prompt}` to `$AURA_ENDPOINT`, returns JSON `reply` or raw body. Without the env var → "engine not connected" notice.
- The engine is **aura-code** (Dule's TypeScript multi-agent framework). Its real request/response shape is not wired yet — ask Dule or read the aura-code repo before changing `ask`.

### Commands (Rust ↔ JS)
| Command | Purpose |
|---|---|
| `ask(prompt)` | send to engine |
| `recent()` | current MRU list |
| `focus_window(id)` | focus a sway container |
| `grab_focus()` | take keyboard into the pane |
| `release_focus()` | give keyboard back to last window |
| `set_mode(auto)` | docked vs auto-hide |
| `set_shown(shown)` | resize surface open/strip |

Events: `aura://focus`, `aura://recent`.

### The rainbow ring (approved design — keep it)
- `@property --aura-a` angle animated by `@keyframes spin`; `conic-gradient` border on `.aura-ring.on` (focused) and `.aura-ring.busy` (thinking, spins at 1.1s).
- `::before` = blurred copy that pulses (`--pulse`, `--blur`).
- Tunables on `:root`: `--spin`, `--pulse`, `--blur`, `--slide`.
- Design reference: claude.ai canvas "Aura OS right pane".

## Status

| Item | State |
|---|---|
| Layer-shell pane, Recent from sway IPC, rainbow ring | done, runs on Aura OS |
| Keyboard focus fix (webview grab) | done — **verify typing works on hardware** |
| Hotkey Super+I | done — verify no conflict |
| Auto-hide + pin toggle | done — **untested on hardware** |
| Engine connection | stub (`AURA_ENDPOINT`) |
| Action cards (installs, settings) | not started |

## Next tasks (in order)

1. **Verify on hardware**: typing, Esc release, Super+I, hover strip, auto-hide timings. Fix anything broken first.
2. **Wider hover zone**: if the 4px strip is hard to hit, make the surface ~8–10px wide while the visible line stays 4px.
3. **Connect aura-code**: match its real API in `ask`; stream tokens if supported (Tauri `Channel`), keep ring in `busy` while streaming.
4. **Action cards** — system actions (pacman install, systemctl, network, audio, display) shown as cards: `[Run] [Edit] [Show command]`.
   - Privileged ops through a small **polkit helper with an allowlist**. Never give the model a raw root shell.
   - Card JSON from engine, e.g. `{"type":"action","kind":"pacman.install","args":["ripgrep"],"preview":"sudo pacman -S ripgrep"}`.
5. **Resume where you left off** (aura-dock feature): on first open after idle, show a "Continue: …" card from last session.
6. **RAM budget**: Dule targets ~9 GB total with a local ~3B model running; keep the pane lean (one webview, no extra processes beyond the swaymsg subscriber).

## Rules

- Keep it Linux/sway only. No cross-platform abstractions.
- No new frontend frameworks or bundlers; vanilla JS stays.
- Run `cargo check` and `node --check src/main.js` before declaring done.
- Deliver complete files.
- Don't commit unless asked.
