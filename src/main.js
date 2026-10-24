const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const root = document.documentElement;
const pane = $("pane");
const ring = $("ring");
const input = $("q");
const feed = $("feed");
const dot = $("dot");
const status = $("status");
const expandBtn = $("expandBtn");
const pinBtn = $("pinBtn");
const recentList = $("recentList");

const SLIDE_MS = 180;
const LEAVE_HIDE_MS = 700;
const REPLY_HIDE_MS = 6000;

const state = {
  focused: false,
  busy: false,
  expanded: false,
  auto: loadAuto(),
  shown: false,
  hovering: false,
  messages: [{ who: "aura", text: "Hi Dule — ask me anything, or press Super + I from any tile." }],
};

let hideTimer = null;
let hideFinish = null;

function loadAuto() {
  try {
    return localStorage.getItem("aura.autohide") !== "0";
  } catch {
    return true;
  }
}

function saveAuto(v) {
  try {
    localStorage.setItem("aura.autohide", v ? "1" : "0");
  } catch {}
}

// ---------- clock ----------
function tick() {
  const now = new Date();
  $("time").textContent = now.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  $("date").textContent = now.toLocaleDateString([], { weekday: "short", day: "numeric", month: "short" });
}
tick();
setInterval(tick, 15000);

// ---------- render ----------
function render() {
  const glow = state.focused || state.busy;
  ring.className = "aura-ring" + (state.busy ? " busy" : state.focused ? " on" : "");
  dot.classList.toggle("live", glow);
  dot.style.background = state.busy ? "var(--think)" : state.focused ? "var(--listen)" : "var(--ready)";
  status.textContent = state.busy ? "thinking…" : state.focused ? "listening" : "local · ready";

  root.classList.toggle("mode-auto", state.auto);
  root.classList.toggle("shown", state.shown);
  pinBtn.setAttribute("aria-pressed", String(!state.auto));
  pinBtn.setAttribute("aria-label", state.auto ? "Pin pane (keep docked)" : "Unpin pane (auto-hide)");

  pane.classList.toggle("expanded", state.expanded);
  expandBtn.setAttribute("aria-expanded", String(state.expanded));
  expandBtn.setAttribute("aria-label", state.expanded ? "Collapse Aura" : "Expand Aura");

  const shown = state.expanded ? state.messages : state.messages.slice(-1);
  feed.replaceChildren(
    ...shown.map((m) => {
      const el = document.createElement("div");
      el.className = "msg " + m.who;
      el.textContent = m.text;
      return el;
    })
  );
  feed.scrollTop = feed.scrollHeight;
}

function renderRecent(items) {
  if (!items || items.length === 0) {
    recentList.innerHTML = '<li class="empty">No windows yet</li>';
    return;
  }
  recentList.replaceChildren(
    ...items.slice(0, 4).map((it) => {
      const li = document.createElement("li");
      const btn = document.createElement("button");
      btn.textContent = it.title ? `${it.app} · ${it.title}` : it.app;
      btn.title = btn.textContent;
      btn.addEventListener("click", () => {
        invoke("focus_window", { id: it.id });
        hide();
      });
      li.appendChild(btn);
      return li;
    })
  );
}

// ---------- show / hide ----------
function cancelHide() {
  clearTimeout(hideTimer);
  clearTimeout(hideFinish);
  hideTimer = null;
  hideFinish = null;
}

async function show() {
  cancelHide();
  if (!state.auto || state.shown) return;
  await invoke("set_shown", { shown: true });
  requestAnimationFrame(() => {
    state.shown = true;
    render();
  });
}

function canHide() {
  if (!state.auto || !state.shown) return false;
  if (state.busy) return false;
  if (state.hovering) return false;
  if (state.focused && input.value.trim() !== "") return false;
  return true;
}

function hide({ force = false } = {}) {
  if (!state.auto || !state.shown) return;
  if (!force && !canHide()) return;
  cancelHide();
  state.shown = false;
  if (state.focused) input.blur();
  render();
  hideFinish = setTimeout(() => invoke("set_shown", { shown: false }), SLIDE_MS);
}

function scheduleHide(ms) {
  clearTimeout(hideTimer);
  hideTimer = setTimeout(() => hide(), ms);
}

async function setAuto(auto) {
  cancelHide();
  state.auto = auto;
  saveAuto(auto);
  if (!auto) state.shown = false;
  await invoke("set_mode", { auto });
  if (auto) {
    state.shown = true;
    await invoke("set_shown", { shown: true });
    render();
    scheduleHide(LEAVE_HIDE_MS);
  } else {
    render();
  }
}

// Hover: the 4px edge strip (and the pane itself) keep it open.
document.body.addEventListener("pointerenter", () => {
  state.hovering = true;
  show();
});
document.body.addEventListener("pointerleave", () => {
  state.hovering = false;
  scheduleHide(LEAVE_HIDE_MS);
});

// ---------- focus ----------
function takeKeyboard() {
  invoke("grab_focus");
  setTimeout(() => input.focus(), 40);
}

$("composer").addEventListener("pointerdown", () => takeKeyboard());

input.addEventListener("focus", () => { state.focused = true; render(); });
input.addEventListener("blur", () => { state.focused = false; render(); });
window.addEventListener("blur", () => {
  if (state.focused) { state.focused = false; render(); }
  scheduleHide(LEAVE_HIDE_MS);
});
input.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    input.blur();
    invoke("release_focus");
    hide({ force: !state.busy });
  }
});

// ---------- send ----------
$("composer").addEventListener("submit", async (e) => {
  e.preventDefault();
  const prompt = input.value.trim();
  if (!prompt || state.busy) return;
  input.value = "";
  state.messages.push({ who: "me", text: prompt });
  state.busy = true;
  render();
  try {
    const reply = await invoke("ask", { prompt });
    state.messages.push({ who: "aura", text: reply });
  } catch (err) {
    state.messages.push({ who: "err", text: "Aura error: " + err });
  } finally {
    state.busy = false;
    render();
    scheduleHide(REPLY_HIDE_MS);
  }
});

expandBtn.addEventListener("click", () => {
  state.expanded = !state.expanded;
  render();
});

pinBtn.addEventListener("click", () => setAuto(!state.auto));

// ---------- backend events ----------
// Super+I: Rust has already slid the pane open and grabbed the keyboard.
listen("aura://focus", () => {
  cancelHide();
  if (state.auto && !state.shown) {
    state.shown = true;
    render();
  }
  setTimeout(() => input.focus(), 40);
});
listen("aura://recent", (e) => renderRecent(e.payload));

// ---------- boot ----------
invoke("recent").then(renderRecent).catch(() => renderRecent([]));
invoke("set_mode", { auto: state.auto });
render();
