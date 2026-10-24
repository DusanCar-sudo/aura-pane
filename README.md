# aura-pane

Right-side Aura pane for Aura OS (Arch + sway). Tauri 2 webview on a wlr-layer-shell surface, docked or auto-hiding, with the rainbow focus ring.

## Install deps (Arch)

```bash
sudo pacman -S --needed webkit2gtk-4.1 gtk3 gtk-layer-shell base-devel rustup nodejs npm
rustup default stable
```

## Build & install

```bash
npm install
npm run build
install -Dm755 src-tauri/target/release/aura-pane ~/.local/bin/aura-pane
cp sway/aura-pane.conf ~/.config/sway/
echo 'include ~/.config/sway/aura-pane.conf' >> ~/.config/sway/config
swaymsg reload
aura-pane &
```

## Behavior

| Trigger | Result |
|---|---|
| Idle (auto-hide) | 4px rainbow strip on the right edge |
| Hover the strip | Pane slides in over tiles |
| `Super+I` | Pane opens, cursor in the input |
| Typing | Rainbow ring spins + pulses |
| Enter | Sends to Aura, ring spins fast while thinking |
| Esc | Keyboard back to last window, pane hides |
| Mouse leaves | Hides after 0.7s (not while typing/thinking) |
| 📌 | Docked (always visible, reserves space) ↔ auto-hide |
| ⤢ | Expands chat, hides Recent |
| Recent item | Focuses that window |

## Engine

```bash
export AURA_ENDPOINT=http://127.0.0.1:7777/chat
```

POST `{"message": "..."}` → shows `reply` from JSON or the raw body.

## Tuning

`src/styles.css` `:root`: `--spin`, `--pulse`, `--blur`, `--slide`. Sizes: `PANE_WIDTH`, `STRIP_WIDTH` in `src-tauri/src/main.rs`.

Developer notes: see `CLAUDE.md`. Handoff steps: `HANDOFF.md`.
