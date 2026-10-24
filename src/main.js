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
const edgeBtn = $("edgeBtn");
const healthBtn = $("health");
const appsList = $("apps");
const recentList = $("recentList");
const systemView = $("systemView");
const graph = $("graph");

const SLIDE_MS = 180;
const LEAVE_HIDE_MS = 700;
const REPLY_HIDE_MS = 6000;
const APPS_MAX = 5;
const HISTORY_DAYS = 7;
const GREETING = { who: "aura", text: "Hi Dule — ask me anything, type an app name, or type \"system\"." };
// Theme file keys → CSS custom properties.
const THEME_VARS = {
  BG: "--bg", SURFACE: "--surface", LINE: "--line", FG: "--fg", DIM: "--dim",
  ACCENT: "--accent", ACCENT2: "--accent2", FONT: "--font", RADIUS: "--radius",
};

const state = {
  focused: false,
  busy: false,
  expanded: false,
  system: false,
  auto: load("aura.autohide", "1") !== "0",
  bottom: load("aura.edge", "right") === "bottom",
  shown: false,
  hovering: false,
  messages: [GREETING],
  apps: [],
  matches: [],
  active: 0,
  history: [],
};

let hideTimer = null;
let hideFinish = null;

function load(key, fallback) {
  try {
    return localStorage.getItem(key) ?? fallback;
  } catch {
    return fallback;
  }
}

function save(key, value) {
  try {
    localStorage.setItem(key, value);
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

// ---------- theme ----------
async function applyTheme() {
  try {
    const theme = await invoke("theme");
    for (const [key, prop] of Object.entries(THEME_VARS)) {
      if (typeof theme[key] === "string" && theme[key]) root.style.setProperty(prop, theme[key]);
    }
    if (state.system) drawGraph();
  } catch (err) {
    console.warn("theme:", err); // keep the built-in ocean-depth defaults
  }
}

// ---------- render ----------
function render() {
  const glow = state.focused || state.busy;
  ring.className = "aura-ring" + (state.busy ? " busy" : state.focused ? " on" : "");
  dot.classList.toggle("live", glow);
  status.textContent = state.busy ? "thinking…" : state.focused ? "listening" : "aura-serve · ready";

  root.classList.toggle("mode-auto", state.auto);
  root.classList.toggle("shown", state.shown);
  root.classList.toggle("edge-bottom", state.bottom);
  pinBtn.setAttribute("aria-pressed", String(!state.auto));
  pinBtn.setAttribute("aria-label", state.auto ? "Pin pane (keep docked)" : "Unpin pane (auto-hide)");
  $("edgeIcon").setAttribute("d", state.bottom ? "M3 15h18" : "M15 4v16");
  edgeBtn.setAttribute("aria-label", state.bottom ? "Move pane to the right side" : "Move pane to the bottom");

  pane.classList.toggle("expanded", state.expanded || state.system);
  pane.classList.toggle("sys", state.system);
  systemView.hidden = !state.system;
  expandBtn.setAttribute("aria-expanded", String(state.expanded));
  expandBtn.setAttribute("aria-label", state.expanded ? "Collapse Aura" : "Expand Aura");

  const shown = state.expanded || state.bottom ? state.messages : state.messages.slice(-1);
  feed.replaceChildren(
    ...shown.map((m) => {
      const el = document.createElement("div");
      el.className = "msg " + m.who;
      el.textContent = m.text;
      return el;
    })
  );
  feed.scrollTop = feed.scrollHeight;
  renderApps();
}

function renderRecent(items) {
  if (!items || items.length === 0) {
    const li = document.createElement("li");
    li.className = "empty";
    li.textContent = "No windows yet";
    recentList.replaceChildren(li);
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
        hide({ force: true });
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
  applyTheme();
  await invoke("set_shown", { shown: true });
  requestAnimationFrame(() => {
    state.shown = true;
    render();
  });
}

function canHide() {
  if (!state.auto || !state.shown) return false;
  if (state.busy || state.hovering) return false;
  if (state.focused && input.value.trim() !== "") return false;
  return true;
}

function hide({ force = false } = {}) {
  if (!state.auto || !state.shown) return;
  if (!force && !canHide()) return;
  cancelHide();
  state.shown = false;
  if (state.focused) input.blur();
  if (state.system) closeSystem();
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
  save("aura.autohide", auto ? "1" : "0");
  state.shown = false;
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

async function setEdge(bottom) {
  try {
    await invoke("set_edge", { edge: bottom ? "bottom" : "right" });
    state.bottom = bottom;
    save("aura.edge", bottom ? "bottom" : "right");
  } catch (err) {
    state.messages.push({ who: "err", text: "Can't move the pane: " + err });
  }
  render();
}

function setExpanded(expanded) {
  state.expanded = expanded;
  invoke("set_expanded", { expanded: expanded || state.system });
  render();
}

// The edge strip and the pane keep it open; leaving hands the keyboard back.
document.body.addEventListener("pointerenter", () => {
  state.hovering = true;
  show();
});
document.body.addEventListener("pointerleave", () => {
  state.hovering = false;
  if (state.focused && !state.busy && input.value.trim() === "") {
    input.blur();
    invoke("release_focus");
  }
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

// ---------- apps ----------
function filterApps(query) {
  const q = query.trim().toLowerCase();
  if (q.length < 2) return [];
  return state.apps
    .filter((a) => a.name.toLowerCase().includes(q) || a.id.toLowerCase().includes(q))
    .sort((a, b) => a.name.toLowerCase().indexOf(q) - b.name.toLowerCase().indexOf(q))
    .slice(0, APPS_MAX);
}

function renderApps() {
  appsList.hidden = state.matches.length === 0;
  appsList.replaceChildren(
    ...state.matches.map((app, i) => {
      const li = document.createElement("li");
      li.setAttribute("role", "option");
      li.setAttribute("aria-selected", String(i === state.active));
      const btn = document.createElement("button");
      btn.type = "button";
      btn.className = i === state.active ? "active" : "";
      btn.textContent = app.name;
      btn.addEventListener("pointerdown", (e) => e.preventDefault()); // keep input focus
      btn.addEventListener("click", () => launchApp(app));
      li.appendChild(btn);
      return li;
    })
  );
}

async function launchApp(app) {
  input.value = "";
  state.matches = [];
  try {
    await invoke("launch", { id: app.id });
    input.blur();
    hide({ force: true });
  } catch (err) {
    state.messages.push({ who: "err", text: `Couldn't start ${app.name}: ${err}` });
  }
  render();
}

input.addEventListener("input", () => {
  state.matches = filterApps(input.value);
  state.active = 0;
  renderApps();
});

input.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    input.blur();
    invoke("release_focus");
    hide({ force: !state.busy });
    return;
  }
  if (state.matches.length === 0) return;
  if (e.key === "ArrowDown" || e.key === "ArrowUp") {
    e.preventDefault();
    const step = e.key === "ArrowDown" ? 1 : -1;
    state.active = (state.active + step + state.matches.length) % state.matches.length;
    renderApps();
  }
});

// ---------- PC health ----------
function level(score) {
  return score >= 80 ? "ok" : score >= 50 ? "warn" : "bad";
}

function showHealth(sample) {
  if (!sample) return;
  healthBtn.textContent = sample.score + "%";
  healthBtn.dataset.level = level(sample.score);
  healthBtn.title = sample.issues.length ? sample.issues.join(" · ") : "All good";
  const last = state.history[state.history.length - 1];
  if (!last || last.ts !== sample.ts) state.history.push(sample);
  if (state.system) renderSystem();
}

async function openSystem() {
  state.system = true;
  invoke("set_expanded", { expanded: true });
  render();
  try {
    state.history = await invoke("health_history", { days: HISTORY_DAYS });
    if (state.history.length === 0) showHealth(await invoke("health_check", { force: true }));
  } catch (err) {
    state.messages.push({ who: "err", text: "Health log unavailable: " + err });
  }
  renderSystem();
}

function closeSystem() {
  state.system = false;
  invoke("set_expanded", { expanded: state.expanded });
  render();
}

function renderSystem() {
  const last = state.history[state.history.length - 1];
  const score = $("systemScore");
  score.textContent = last ? last.score + "%" : "--%";
  score.style.color = last ? `var(--${level(last.score)})` : "";
  const issues = $("healthIssues");
  const items = last && last.issues.length ? last.issues : ["Everything looks fine"];
  issues.replaceChildren(
    ...items.map((text) => {
      const li = document.createElement("li");
      li.textContent = text;
      if (!last || last.issues.length === 0) li.className = "fine";
      return li;
    })
  );
  drawGraph();
}

function hexToRgb(hex) {
  const h = hex.trim().replace("#", "");
  const full = h.length === 3 ? [...h].map((c) => c + c).join("") : h.slice(0, 6);
  const n = parseInt(full, 16);
  return Number.isNaN(n) ? [128, 128, 128] : [(n >> 16) & 255, (n >> 8) & 255, n & 255];
}

// btop-style 3-stop gradient: ok → warn → bad by height.
function gradient(stops, t) {
  const [a, b, u] = t < 0.5 ? [stops[0], stops[1], t * 2] : [stops[1], stops[2], (t - 0.5) * 2];
  return `rgb(${a.map((v, i) => Math.round(v + (b[i] - v) * u)).join(",")})`;
}

// Series values are 0..1; `good` flips the colours (high health = green).
const SERIES = [
  { title: "health", get: (s) => s.score / 100, label: (s) => s.score + "%", good: true },
  { title: "cpu", get: (s) => s.cpu / 100, label: (s) => s.cpu.toFixed(0) + "%" },
  { title: "mem", get: (s) => s.mem / 100, label: (s) => s.mem.toFixed(0) + "%" },
  { title: "temp", get: (s) => s.temp / 100, label: (s) => (s.temp > 0 ? s.temp.toFixed(0) + "°C" : "n/a") },
];

function drawGraph() {
  const dpr = window.devicePixelRatio || 1;
  const w = graph.clientWidth;
  const h = graph.clientHeight;
  if (!w || !h) return;
  graph.width = Math.round(w * dpr);
  graph.height = Math.round(h * dpr);
  const ctx = graph.getContext("2d");
  ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
  ctx.clearRect(0, 0, w, h);

  const css = getComputedStyle(root);
  const color = (name) => css.getPropertyValue(name).trim();
  const stops = ["--ok", "--warn", "--bad"].map((n) => hexToRgb(color(n)));
  const font = color("--font") || "monospace";
  const data = state.history;
  const gap = 8;
  const panelH = (h - gap * (SERIES.length + 1)) / SERIES.length;

  SERIES.forEach((series, row) => {
    const x0 = gap;
    const y0 = gap + row * (panelH + gap);
    const pw = w - gap * 2;
    ctx.strokeStyle = color("--line");
    ctx.lineWidth = 1;
    ctx.strokeRect(x0 + 0.5, y0 + 0.5, pw - 1, panelH - 1);

    ctx.font = `600 10px ${font}`;
    ctx.textBaseline = "middle";
    const title = ` ${series.title} `;
    const titleW = ctx.measureText(title).width;
    ctx.fillStyle = color("--bg");
    ctx.fillRect(x0 + 8, y0 - 6, titleW, 12);
    ctx.fillStyle = color("--accent");
    ctx.fillText(title, x0 + 8, y0);
    const last = data[data.length - 1];
    if (last) {
      const value = ` ${series.label(last)} `;
      const vw = ctx.measureText(value).width;
      ctx.fillStyle = color("--bg");
      ctx.fillRect(x0 + pw - 8 - vw, y0 - 6, vw, 12);
      ctx.fillStyle = color("--fg");
      ctx.fillText(value, x0 + pw - 8 - vw, y0);
    }
    if (data.length === 0) return;

    // One column per sample (averaged when there are more samples than columns).
    const innerX = x0 + 4;
    const innerY = y0 + 8;
    const innerW = pw - 8;
    const innerH = panelH - 12;
    const colW = 3;
    const cols = Math.max(1, Math.floor(innerW / colW));
    const used = Math.min(cols, data.length);
    const cell = 3;
    const rows = Math.max(1, Math.floor(innerH / cell));
    for (let c = 0; c < used; c++) {
      const from = Math.floor((c * data.length) / used);
      const to = Math.max(from + 1, Math.floor(((c + 1) * data.length) / used));
      let sum = 0;
      for (let i = from; i < to; i++) sum += series.get(data[i]);
      const v = Math.min(1, Math.max(0, sum / (to - from)));
      const filled = Math.max(v > 0 ? 1 : 0, Math.round(v * rows));
      const x = innerX + innerW - (used - c) * colW;
      for (let r = 0; r < filled; r++) {
        const t = (r + 1) / rows;
        ctx.fillStyle = gradient(stops, series.good ? 1 - t : t);
        ctx.fillRect(x, innerY + innerH - (r + 1) * cell, colW - 1, cell - 1);
      }
    }
  });
}

new ResizeObserver(() => { if (state.system) drawGraph(); }).observe(graph);

healthBtn.addEventListener("click", () => (state.system ? closeSystem() : openSystem()));
$("systemClose").addEventListener("click", () => closeSystem());

// ---------- send ----------
function chatHistory() {
  return state.messages
    .filter((m) => m !== GREETING && (m.who === "me" || m.who === "aura"))
    .map((m) => ({ role: m.who === "me" ? "user" : "assistant", content: m.text }));
}

$("composer").addEventListener("submit", async (e) => {
  e.preventDefault();
  const prompt = input.value.trim();
  if (!prompt || state.busy) return;

  if (/^(system|health)$/i.test(prompt)) {
    input.value = "";
    openSystem();
    return;
  }
  if (state.matches.length > 0) {
    launchApp(state.matches[state.active]);
    return;
  }

  // "show me a graph", "how is the system doing": open the graph and let Aura explain it.
  if (/\b(graph|chart)\b|\b(system|pc|computer)\b.*\b(state|health|status|doing)\b/i.test(prompt)) openSystem();

  input.value = "";
  state.messages.push({ who: "me", text: prompt });
  state.busy = true;
  render();
  try {
    const reply = await invoke("ask", { messages: chatHistory() });
    state.messages.push({ who: "aura", text: reply });
  } catch (err) {
    state.messages.push({ who: "err", text: "Aura error: " + err });
  } finally {
    state.busy = false;
    render();
    scheduleHide(REPLY_HIDE_MS);
  }
});

expandBtn.addEventListener("click", () => setExpanded(!state.expanded));
pinBtn.addEventListener("click", () => setAuto(!state.auto));
edgeBtn.addEventListener("click", () => setEdge(!state.bottom));

// ---------- backend events ----------
// Super+I: Rust has already slid the pane open and grabbed the keyboard.
listen("aura://focus", () => {
  cancelHide();
  applyTheme();
  if (state.auto && !state.shown) {
    state.shown = true;
    render();
  }
  setTimeout(() => input.focus(), 40);
});
listen("aura://recent", (e) => renderRecent(e.payload));
listen("aura://health", (e) => showHealth(e.payload));
// Music playing: the edges pulse with the beat (audio.rs) instead of breathing.
listen("aura://music", (e) => root.classList.toggle("music", e.payload));
listen("aura://beat", (e) => root.style.setProperty("--beat", e.payload.toFixed(3)));

// ---------- boot ----------
applyTheme();
invoke("recent").then(renderRecent).catch(() => renderRecent([]));
invoke("apps").then((apps) => { state.apps = apps; }).catch((err) => console.warn("apps:", err));
invoke("health_check", { force: false }).then(showHealth).catch((err) => console.warn("health:", err));
invoke("set_mode", { auto: state.auto });
if (state.bottom) setEdge(true);
render();
