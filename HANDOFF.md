# Handoff → Claude Code

## 1. Start Claude Code in the project

```bash
cd ~/projects/aura-pane
git init && git add -A && git commit -m "aura-pane v0.2: layer-shell pane, auto-hide, rainbow ring"
claude
```

Claude Code reads `CLAUDE.md` automatically — it has the full architecture, status and next tasks.

## 2. Paste this as the first message

```
Read CLAUDE.md. This is aura-pane v0.2, running on my Aura OS (Arch + sway).

First, help me verify on hardware:
1. Build and install it (npm run build, install to ~/.local/bin, restart).
2. Run it from the terminal so we see logs.
3. Walk me through the test list: typing in the box, Esc, Super+I,
   hover the edge strip, auto-hide timings, pin toggle.

Fix whatever is broken, then continue with "Next tasks" in CLAUDE.md
in order. Ask me before wiring the aura-code engine API.
Deliver complete files, run cargo check + node --check before saying done.
```

## 3. Useful when debugging

```bash
pkill aura-pane; aura-pane                 # run with logs
swaymsg -t get_tree | less                 # see window tree
swaymsg -t subscribe -m '["window"]'       # watch focus events live
grep -rn "mod+i" ~/.config/sway            # check hotkey conflicts
```

WebKit inspector (dev builds): right-click in the pane → Inspect Element.
