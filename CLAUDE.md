# CLAUDE.md — aura-pane

Aura pane for **Aura OS** (Arch Linux + sway, Wayland). Tauri 2 app whose webview lives on a **wlr-layer-shell** surface anchored to the right edge or the bottom edge. Linux/sway only — no Windows/macOS targets.

Owner: Dule. Style: concise, direct. Always deliver complete files, never partial snippets.

## Commands

```bash
npm install                 # once
npm run dev                 # dev build + run
npm run build               # release → src-tauri/target/release/aura-pane
cd src-tauri && cargo check && cargo test   # Rust compile check + unit tests
node --check src/main.js    # JS syntax check
install -Dm755 src-tauri/target/release/aura-pane ~/.local/bin/aura-pane
kill $(pgrep -x aura-pane); aura-pane   # restart with logs in the terminal
```

System deps: `webkit2gtk-4.1 gtk3 gtk-layer-shell nodejs npm rustup base-devel`.

## Layout

```
src/                    frontend — vanilla HTML/CSS/JS, no bundler
  index.html            clock, Recent, PC-health view, Aura box, app list, buttons, edge strip
  styles.css            theme tokens on :root, glass pane, rainbow edge + ring, slide, graph/issues
  main.js               state, render(), show/hide, theme, apps, system graph, Tauri invoke/listen
src-tauri/
  src/main.rs           layer shell, layout, keyboard focus, sway IPC, theme, apps, engine, commands
  src/health.rs         PC health sample + score, health.jsonl log
  src/audio.rs          music-reactive edge: parec monitor tap → beat level
  tauri.conf.json       window "main" (hidden, transparent, undecorated), withGlobalTauri
  capabilities/default.json
sway/aura-pane.conf     exec + Super+I binding (symlinked into ~/.config/sway/config.d/)
```

## Architecture

### Layer shell (main.rs `setup_layer_shell`)
- Tauri's GTK window is already realized, so gtk-layer-shell can't be initialized on it. We create a **new `gtk::ApplicationWindow`** (namespace `aura-pane`, `Layer::Top`, RGBA visual), `init_layer_shell()` it, **reparent the webview** into it, then hide the original.
- GTK objects are `!Send` → stored in `thread_local!`. Touch them only on the main thread via `app.run_on_main_thread`.
- Name collisions: use `set_layer_shell_margin` (not `set_margin`) and `WidgetExt::screen(&w)` explicitly.

### Layout (`Layout { auto, shown, bottom, expanded }`, `apply_layout()`)
- Sizes: pane 360 wide (right) / 300 tall (bottom); expanded 760×640; `GLOW_PAD` 14px extra on the inner side so the rainbow edge glow isn't clipped; hidden strip `STRIP` 8px.
- Inner side (left in right mode, top in bottom mode) is unanchored with margin 0; other edges get `MARGIN` 10 when open.
- **Docked**: `auto_exclusive_zone_enable()` → tiles reflow. **Auto**: exclusive zone `STRIP`; open pane overlays tiles.
- JS removes `.shown`, waits `SLIDE_MS`, then calls `set_shown(false)` so the CSS slide finishes before the surface shrinks.
- localStorage: `aura.autohide` (default auto), `aura.edge` (`right`|`bottom`). JS calls `set_mode` / `set_edge` on boot.

### Keyboard focus (root cause + fix)
- sway's `arrange_layers` drops focus from an **OnDemand** layer surface on any layer commit (e.g. our resize) → cursor showed, then jumped back to the last window.
- **Grab** (`grab_focus`, click in composer, Super+I) = `KeyboardMode::Exclusive` + `present()` + focus the WebKitWebView. No timed revert.
- **Release** (`release_focus`, Esc, pointer leaves with an empty input, `set_shown(false)`) = back to `OnDemand`.
- `focus_window` releases, waits 60ms, then `swaymsg [con_id=N] focus`.

### Theme
- `~/.config/aura/theme` names the theme; `~/.config/aura/themes/<name>.conf` holds `KEY="value"` lines.
- `theme()` returns BG, SURFACE, LINE, FG, DIM, ACCENT, ACCENT2, FONT, RADIUS → JS maps them to `--bg … --radius`. Applied on boot and on every show, so a theme switch lands next time the pane opens.
- CSS keeps ocean-depth defaults if the file is missing.

### Glass + rainbow edge
- `.pane` = translucent `--bg` at 86% (lower and text behind bleeds through) + sheen gradient + `--glass-line` border. **No backdrop-filter**: a layer surface has nothing behind it to blur.
- Right mode: `.pane::before/::after` = 3px rainbow line + blurred halo on the **left** edge, flowing (`flow-y` 14s) and breathing (`--breathe` 11s: dim most of the time, swells to full at ~72%).
- Bottom mode: same on the **top** edge with `flow-x`, always softly lit (`glow` 6s, .5↔.72 opacity).
- Retracted strip `.edge`: 4px, `edge-breathe` 4s (.3↔1 opacity + accent halo) so it is visible from across the screen.
- **Music** (`audio.rs`): every 2s `pactl -f json list sink-inputs`; if a stream is uncorked, `parec -d @DEFAULT_MONITOR@` (8kHz mono) → bass-weighted onset level → `aura://beat` ~30/s + `aura://music` on/off. `html.music` swaps the breathing for `--beat`-driven opacity/halo/glow on every edge. parec is killed once nothing plays, so the sink can suspend.
- Rainbow ring around the composer (approved design — keep it): `@property --aura-a` + `conic-gradient`, `.on` when focused, `.busy` spins fast while thinking.

### Recent windows
- `seed_recent`: `swaymsg -r -t get_tree`. `watch_sway`: `swaymsg -r -m -t subscribe '["window"]'` → emits `aura://recent`.

### Apps
- `apps()` scans XDG `applications/*.desktop` (skips NoDisplay/Hidden). JS filters by name/id substring (2+ chars, max 5); ↑/↓ + Enter or click → `launch(id)` → `gtk-launch <id>`, only for ids in the scanned list.

### PC health (`health.rs`)
- `check(force)`: CPU, mem, swap, disk (one row per device — btrfs subvolumes deduped), max temp, failed systemd units, journal errors in the last hour → `score` 0–100 + plain-language `issues`.
- Throttled to 5 min unless forced. Runs after each `ask` reply and every `HEALTH_EVERY` (1h). Appends to `~/.local/share/aura-pane/health.jsonl`.
- UI: `--%` badge in the Aura header (ok ≥80, warn ≥50, else bad). Typing `system` or `health` + Enter, or clicking the badge, expands the pane to a btop-style graph (health, cpu, mem, temp; 3px cells green→yellow→red, from aura-sysmon's `btop_graph`) over the last 7 days + the issue list.

### Engine
- `ask(messages)` → POST `$AURA_ENDPOINT` (default `http://127.0.0.1:7337/v1/chat/completions`, the always-on aura-serve) with `{"messages":[...]}`; reply = `choices[0].message.content`.
- Bearer token read at call time from `$AURA_TOKEN` or `~/.aura/server-token` — never hardcode it. Timeout 180s, no streaming.
- JS sends the last turns (greeting and error lines dropped); Rust keeps the last 12 user/assistant messages.
- Rust prepends a **system prompt** (`system_prompt`): Aura persona, what the pane can do (apps, `system` graph), "can't run commands yet", and the latest health sample. Without it aura-serve's `/v1/chat/completions` answers as a bare model.
- JS opens the graph for natural requests too ("graph", "chart", "how is the system doing") and still sends them to Aura to explain.

### Hotkey
- `Super+I` → `aura-pane --focus` → single-instance plugin → `focus_input()` shows, grabs keyboard, emits `aura://focus`.
- **Taken keys on Dule's setup — don't use:** `Super+Space` (settings), `Super+A` (Aura terminal).

### Commands (Rust ↔ JS)
| Command | Purpose |
|---|---|
| `ask(messages)` | chat with aura-serve |
| `recent()` | current MRU list |
| `focus_window(id)` | focus a sway container |
| `grab_focus()` / `release_focus()` | take / give back the keyboard |
| `set_mode(auto)` | docked vs auto-hide |
| `set_shown(shown)` | open surface vs strip |
| `set_edge(edge)` | `right` or `bottom` |
| `set_expanded(expanded)` | big size for chat history / system graph |
| `theme()` | current theme keys |
| `apps()` / `launch(id)` | app list / start one |
| `health_check(force)` / `health_history(days)` | one sample / log |

Events: `aura://focus`, `aura://recent`, `aura://health`, `aura://music`, `aura://beat`.

## Status

| Item | State |
|---|---|
| Layer-shell pane, Recent, rainbow ring, Super+I | done, runs on Aura OS |
| Focus fix (Exclusive grab, OnDemand release) | done — **verify on hardware** |
| Engine → aura-serve :7337 | done, verified from the CLI |
| Theme colours, glass, rainbow edge (right + bottom) | done — check visually |
| Edge pulses to music | done — **untested with real playback** |
| Bottom mode, app launcher, PC health + system graph | done — **untested on hardware** |
| Action cards, resume card, streaming | not started |

## Next tasks

1. **Verify on hardware**: typing/click focus, Esc, hover strip, both edges, app launch, `system` view.
2. **Streaming replies** (Tauri `Channel`), ring stays `busy` while streaming.
3. **Action cards** — `[Run] [Edit] [Show command]`; privileged ops through a polkit helper with an allowlist, never a raw root shell.
4. **Resume card**: on first open after idle, "Continue: …" from the last session.
5. **health.jsonl rotation** (it grows ~9k lines/year at 1/hour + per-task).
6. **RAM budget**: keep it one webview, no extra processes beyond the swaymsg subscriber.

## Rules

- Linux/sway only. No new frontend frameworks or bundlers; vanilla JS stays.
- Run `cargo check`, `cargo test` and `node --check src/main.js` before declaring done.
- Deliver complete files. Don't commit unless asked.
