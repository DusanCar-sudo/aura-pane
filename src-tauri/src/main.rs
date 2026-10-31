// Aura pane — layer-shell panel for Aura OS (sway / Wayland), right or bottom edge.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;
mod health;

use gtk::glib;
use gtk::prelude::*;
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

const PANE_W: i32 = 360;
const PANE_H: i32 = 300;
const EXPANDED_W: i32 = 760;
const EXPANDED_H: i32 = 640;
// Transparent band on the pane's inner side for the edge glow; = --glow-pad in styles.css.
const GLOW_PAD: i32 = 14;
// Hidden auto-hide strip: the visible line is 3px, the hover target 8px.
const STRIP: i32 = 8;
const MARGIN: i32 = 10;
const RECENT_MAX: usize = 8;
const HISTORY_MAX: usize = 12;
const HISTORY_DAYS_MAX: u64 = 90;
const ENGINE_URL: &str = "http://127.0.0.1:7337/v1/chat/completions";
const ENGINE_TIMEOUT: Duration = Duration::from_secs(180);
const HEALTH_EVERY: Duration = Duration::from_secs(3600);
// Let sway apply the keyboard release before focusing another window.
const RELEASE_SETTLE: Duration = Duration::from_millis(60);

#[derive(Clone, Copy)]
struct Layout {
    auto: bool,
    shown: bool,
    bottom: bool,
    expanded: bool,
}

thread_local! {
    // GTK objects are !Send: they live on the main thread only.
    static PANE: RefCell<Option<gtk::ApplicationWindow>> = RefCell::new(None);
    static WEBVIEW: RefCell<Option<gtk::Widget>> = RefCell::new(None);
    // Invisible full-screen layers, one per monitor, under the open pane: a click on one = clicked outside.
    static CATCHERS: RefCell<Vec<gtk::Window>> = RefCell::new(Vec::new());
    static APP: RefCell<Option<AppHandle>> = RefCell::new(None);
    static LAYOUT: Cell<Layout> = Cell::new(Layout {
        auto: true,
        shown: false,
        bottom: false,
        expanded: false,
    });
}

#[derive(Clone, Serialize)]
struct RecentItem {
    id: i64,
    app: String,
    title: String,
}

struct Recent(Mutex<Vec<RecentItem>>);

#[derive(Clone, Serialize)]
struct AppEntry {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct ChatMsg {
    role: String,
    content: String,
}

// ---------- layer shell ----------

fn find_webview(w: &gtk::Widget) -> Option<gtk::Widget> {
    if w.type_().name().contains("WebKitWebView") {
        return Some(w.clone());
    }
    if let Some(c) = w.downcast_ref::<gtk::Container>() {
        for child in c.children() {
            if let Some(found) = find_webview(&child) {
                return Some(found);
            }
        }
    }
    None
}

fn focus_webview() {
    WEBVIEW.with(|v| {
        if let Some(v) = v.borrow().as_ref() {
            v.set_can_focus(true);
            v.grab_focus();
        }
    });
}

/// Docked: full pane, reserves its space.
/// Auto-hide: tiles keep the full screen minus the strip; when shown,
/// the pane slides over the tiles without reflowing them.
/// Right edge: anchored top/bottom/right, sized by width.
/// Bottom edge: anchored left/right/bottom, sized by height.
fn apply_layout() {
    let l = LAYOUT.with(|c| c.get());
    let open = !l.auto || l.shown;
    let size = match (open, l.bottom, l.expanded) {
        (false, _, _) => STRIP,
        (true, false, false) => PANE_W + GLOW_PAD,
        (true, false, true) => EXPANDED_W + GLOW_PAD,
        (true, true, false) => PANE_H + GLOW_PAD,
        (true, true, true) => EXPANDED_H + GLOW_PAD,
    };
    let margin = if open { MARGIN } else { 0 };
    // Slid out over the tiles: lift the pane to Overlay so the catcher (Top) sits below it.
    let catch = l.auto && l.shown;
    PANE.with(|p| {
        let Some(w) = p.borrow().clone() else { return };
        w.set_layer(if catch { Layer::Overlay } else { Layer::Top });
        for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
            // The side facing the screen stays free.
            let inner = matches!((edge, l.bottom), (Edge::Top, true) | (Edge::Left, false));
            w.set_anchor(edge, !inner);
            w.set_layer_shell_margin(edge, if inner { 0 } else { margin });
        }
        if l.auto {
            w.set_exclusive_zone(STRIP);
        } else {
            w.auto_exclusive_zone_enable();
        }
        if l.bottom {
            w.set_size_request(-1, size);
            w.resize(1, size);
        } else {
            w.set_size_request(size, -1);
            w.resize(size, 1);
        }
    });
    sync_catchers(l.auto && l.shown);
}

fn sync_catchers(show: bool) {
    CATCHERS.with(|c| {
        let mut list = c.borrow_mut();
        if !show {
            list.iter().for_each(|w| w.hide());
            return;
        }
        let Some(display) = gtk::gdk::Display::default() else { return };
        let monitors: Vec<_> = (0..display.n_monitors()).filter_map(|i| display.monitor(i)).collect();
        // shortcut: rebuilt only when the monitor count changes, not on a same-count swap.
        if list.len() != monitors.len() {
            list.drain(..).for_each(|w| w.close());
            let Some(app) = APP.with(|a| a.borrow().clone()) else { return };
            *list = monitors.into_iter().map(|m| build_catcher(app.clone(), m)).collect();
        }
        list.iter().for_each(|w| w.show_all());
    });
}

fn update_layout(f: impl FnOnce(Layout) -> Layout) {
    LAYOUT.with(|c| c.set(f(c.get())));
    apply_layout();
}

fn setup_layer_shell(win: &WebviewWindow) -> Result<(), Box<dyn std::error::Error>> {
    let old = win.gtk_window()?;
    let gtk_app = old.application().ok_or("no gtk application")?;

    // The Tauri window is already realized, so build a fresh window,
    // make it a layer surface, and move the webview into it.
    let pane = gtk::ApplicationWindow::new(&gtk_app);
    pane.set_app_paintable(true);
    if let Some(screen) = WidgetExt::screen(&pane) {
        if let Some(visual) = screen.rgba_visual() {
            pane.set_visual(Some(&visual));
        }
    }

    pane.init_layer_shell();
    pane.set_namespace("aura-pane");
    pane.set_layer(Layer::Top);
    pane.set_keyboard_mode(KeyboardMode::OnDemand);

    // Move only the bin child: children() also lists tao's dummy titlebar
    // (a GtkEventBox), and a GtkBin can't hold a second widget.
    let mut webview = None;
    if let Some(child) = old.child() {
        old.remove(&child);
        pane.add(&child);
        webview = find_webview(&child);
    }

    // Whenever sway hands the surface the keyboard, pass it to the webview.
    pane.connect_focus_in_event(|_, _| {
        focus_webview();
        glib::Propagation::Proceed
    });

    WEBVIEW.with(|v| *v.borrow_mut() = webview);
    APP.with(|a| *a.borrow_mut() = Some(win.app_handle().clone()));
    PANE.with(|p| *p.borrow_mut() = Some(pane.clone()));
    apply_layout();

    pane.show_all();
    old.hide();
    Ok(())
}

/// Transparent, keyboard-less, covers the whole output (bar included).
/// One click anywhere hides them all first, so a stuck catcher can never eat more than one click;
/// the window under that click still gets focused.
fn build_catcher(app: AppHandle, monitor: gtk::gdk::Monitor) -> gtk::Window {
    let catcher = gtk::Window::new(gtk::WindowType::Toplevel);
    catcher.set_app_paintable(true);
    if let Some(visual) = WidgetExt::screen(&catcher).and_then(|s| s.rgba_visual()) {
        catcher.set_visual(Some(&visual));
    }
    catcher.connect_draw(|_, cr| {
        cr.set_operator(gtk::cairo::Operator::Clear);
        let _ = cr.paint();
        glib::Propagation::Stop
    });
    catcher.init_layer_shell();
    catcher.set_namespace("aura-pane-catcher");
    catcher.set_layer(Layer::Top);
    catcher.set_keyboard_mode(KeyboardMode::None);
    catcher.set_exclusive_zone(-1);
    catcher.set_monitor(&monitor);
    for edge in [Edge::Top, Edge::Bottom, Edge::Left, Edge::Right] {
        catcher.set_anchor(edge, true);
    }
    catcher.add_events(gtk::gdk::EventMask::BUTTON_PRESS_MASK);
    catcher.connect_button_press_event(move |_, ev| {
        sync_catchers(false);
        let geo = monitor.geometry();
        let (x, y) = ev.position();
        let (x, y) = (geo.x() as i64 + x as i64, geo.y() as i64 + y as i64);
        let app = app.clone();
        let _ = app.emit("aura://outside", ());
        std::thread::spawn(move || {
            let tree = Command::new("swaymsg").args(["-r", "-t", "get_tree"]).output();
            let tree = tree.ok().and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok());
            if let Some(id) = tree.and_then(|t| window_at(&t, x, y)) {
                focus_window(app, id);
            }
        });
        glib::Propagation::Stop
    });
    catcher
}

/// sway drops an OnDemand layer's keyboard focus on every arrange_layers
/// (any layer commit — ours, mako's, a resize), which threw typing back to
/// the last window. So the pane holds Exclusive for as long as it wants keys,
/// and releasing = back to OnDemand, where sway refocuses the previous window.
/// Main thread only.
fn set_keyboard(exclusive: bool) {
    PANE.with(|p| {
        if let Some(w) = p.borrow().as_ref() {
            let mode = if exclusive { KeyboardMode::Exclusive } else { KeyboardMode::OnDemand };
            w.set_keyboard_mode(mode);
            if exclusive {
                w.present();
            }
        }
    });
    if exclusive {
        focus_webview();
    }
}

fn grab_keyboard(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| set_keyboard(true));
}

fn release_keyboard(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| set_keyboard(false));
}

fn focus_input(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| update_layout(|l| Layout { shown: true, ..l }));
    grab_keyboard(app);
    let _ = app.emit("aura://focus", ());
}

// ---------- sway recent windows ----------

fn item_from(c: &Value) -> Option<RecentItem> {
    let id = c["id"].as_i64()?;
    let app = c["app_id"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| c["window_properties"]["class"].as_str())
        .unwrap_or("app")
        .to_string();
    let title = c["name"].as_str().unwrap_or("").to_string();
    Some(RecentItem { id, app, title })
}

fn collect(node: &Value, out: &mut Vec<(bool, RecentItem)>) {
    let is_window = node["pid"].as_i64().is_some()
        && matches!(node["type"].as_str(), Some("con") | Some("floating_con"));
    if is_window {
        if let Some(item) = item_from(node) {
            out.push((node["focused"].as_bool().unwrap_or(false), item));
        }
    }
    for key in ["nodes", "floating_nodes"] {
        if let Some(children) = node[key].as_array() {
            for child in children {
                collect(child, out);
            }
        }
    }
}

/// The visible window under a global layout point; floating wins over tiled (drawn on top).
fn window_at(node: &Value, x: i64, y: i64) -> Option<i64> {
    let r = &node["rect"];
    let inside = |k: &str, len: &str, v: i64| {
        let start = r[k].as_i64().unwrap_or(0);
        v >= start && v < start + r[len].as_i64().unwrap_or(0)
    };
    let mut hit = None;
    if node["pid"].as_i64().is_some() && node["visible"] == true && inside("x", "width", x) && inside("y", "height", y) {
        hit = node["id"].as_i64();
    }
    for key in ["nodes", "floating_nodes"] {
        for child in node[key].as_array().into_iter().flatten() {
            hit = window_at(child, x, y).or(hit);
        }
    }
    hit
}

fn seed_recent(app: &AppHandle) {
    let Ok(out) = Command::new("swaymsg").args(["-r", "-t", "get_tree"]).output() else {
        return;
    };
    let Ok(tree) = serde_json::from_slice::<Value>(&out.stdout) else {
        return;
    };
    let mut found = Vec::new();
    collect(&tree, &mut found);
    found.sort_by_key(|(focused, _)| !*focused);
    let state = app.state::<Recent>();
    let mut list = state.0.lock().unwrap();
    *list = found.into_iter().map(|(_, i)| i).take(RECENT_MAX).collect();
}

fn watch_sway(app: AppHandle) {
    std::thread::spawn(move || {
        let Ok(mut child) = Command::new("swaymsg")
            .args(["-r", "-m", "-t", "subscribe", "[\"window\"]"])
            .stdout(Stdio::piped())
            .spawn()
        else {
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            return;
        };
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let Ok(ev) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            let Some(item) = item_from(&ev["container"]) else {
                continue;
            };
            let state = app.state::<Recent>();
            // sway re-sends "focus" for the same window when the pane grabs the keyboard; only a new window counts.
            let moved = ev["change"] == "focus" && state.0.lock().unwrap().first().map(|i| i.id) != Some(item.id);
            let snapshot = {
                let mut list = state.0.lock().unwrap();
                match ev["change"].as_str().unwrap_or("") {
                    "focus" => {
                        list.retain(|i| i.id != item.id);
                        list.insert(0, item);
                        list.truncate(RECENT_MAX);
                    }
                    "close" => list.retain(|i| i.id != item.id),
                    "title" => {
                        if let Some(existing) = list.iter_mut().find(|i| i.id == item.id) {
                            *existing = item;
                        }
                    }
                    _ => continue,
                }
                list.clone()
            };
            let _ = app.emit("aura://recent", snapshot);
            // Another window took focus (keyboard switch or click outside the pane): retract.
            if moved {
                let _ = app.emit("aura://outside", ());
            }
        }
    });
}

// ---------- theme ----------

fn config_dir() -> Option<PathBuf> {
    std::env::var("XDG_CONFIG_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".config")))
}

/// aura-os-theme writes shell-style `KEY="value"` lines.
fn parse_kv(text: &str) -> HashMap<String, String> {
    text.lines()
        .filter_map(|line| {
            let (k, v) = line.trim().split_once('=')?;
            let key_ok = !k.is_empty()
                && k.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
            key_ok.then(|| (k.to_string(), v.trim().trim_matches('"').trim_matches('\'').to_string()))
        })
        .collect()
}

fn valid_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// ---------- apps ----------

fn app_dirs() -> Vec<PathBuf> {
    let data_home = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    data_home
        .into_iter()
        .chain(data_dirs.split(':').map(PathBuf::from))
        .map(|d| d.join("applications"))
        .collect()
}

/// Name of a launchable [Desktop Entry], or None if hidden / not an app.
fn desktop_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    let mut name = None;
    let mut is_app = false;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        match line.split_once('=').map(|(k, v)| (k.trim(), v.trim())) {
            Some(("Name", v)) => name = Some(v.to_string()),
            Some(("Type", v)) => is_app = v == "Application",
            Some(("NoDisplay" | "Hidden", "true")) => return None,
            _ => {}
        }
    }
    name.filter(|_| is_app)
}

fn scan_apps() -> Vec<AppEntry> {
    let mut seen = HashSet::new();
    let mut apps = Vec::new();
    // User dirs come first, so their overrides win.
    for dir in app_dirs() {
        let Ok(entries) = fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            let file = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = file.strip_suffix(".desktop") else { continue };
            if !seen.insert(id.to_string()) {
                continue;
            }
            let Ok(text) = fs::read_to_string(entry.path()) else { continue };
            if let Some(name) = desktop_name(&text) {
                apps.push(AppEntry { id: id.to_string(), name });
            }
        }
    }
    apps.sort_by_key(|a| a.name.to_lowercase());
    apps
}

// ---------- engine (aura-serve) ----------

fn engine_token() -> Result<String, String> {
    if let Ok(t) = std::env::var("AURA_TOKEN") {
        if !t.trim().is_empty() {
            return Ok(t.trim().to_string());
        }
    }
    let home = std::env::var("HOME").map_err(|_| "HOME is not set".to_string())?;
    fs::read_to_string(PathBuf::from(home).join(".aura/server-token"))
        .map(|t| t.trim().to_string())
        .map_err(|e| format!("Can't read ~/.aura/server-token ({e}) — is aura-serve set up?"))
}

fn parse_reply(v: &Value) -> Result<String, String> {
    if let Some(text) = v["choices"][0]["message"]["content"].as_str() {
        return Ok(text.to_string());
    }
    if let Some(msg) = v["error"]["message"].as_str() {
        return Err(format!("Engine: {msg}"));
    }
    Err("Engine sent a reply I can't read.".into())
}

/// After each task Aura takes one look around the PC (throttled in health).
fn after_task(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let _ = app.emit("aura://health", health::check(false));
    });
}

fn watch_health(app: AppHandle) {
    std::thread::spawn(move || loop {
        let _ = app.emit("aura://health", health::check(false));
        std::thread::sleep(HEALTH_EVERY);
    });
}

// ---------- commands ----------

#[tauri::command]
fn recent(state: tauri::State<Recent>) -> Vec<RecentItem> {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn focus_window(app: AppHandle, id: i64) {
    release_keyboard(&app);
    std::thread::spawn(move || {
        std::thread::sleep(RELEASE_SETTLE);
        if let Err(e) = Command::new("swaymsg").arg(format!("[con_id={id}] focus")).status() {
            eprintln!("aura-pane: swaymsg focus failed: {e}");
        }
    });
}

#[tauri::command]
fn grab_focus(app: AppHandle) {
    grab_keyboard(&app);
}

/// Give the keyboard back; sway refocuses the previous window.
#[tauri::command]
fn release_focus(app: AppHandle) {
    release_keyboard(&app);
}

#[tauri::command]
fn set_mode(app: AppHandle, auto: bool) {
    let _ = app.run_on_main_thread(move || {
        update_layout(|l| Layout { auto, shown: false, ..l })
    });
}

#[tauri::command]
fn set_shown(app: AppHandle, shown: bool) {
    let _ = app.run_on_main_thread(move || {
        if !shown {
            set_keyboard(false);
        }
        update_layout(|l| Layout { shown, ..l });
    });
}

#[tauri::command]
fn set_edge(app: AppHandle, edge: String) -> Result<(), String> {
    let bottom = match edge.as_str() {
        "right" => false,
        "bottom" => true,
        other => return Err(format!("unknown edge {other:?}")),
    };
    let _ = app.run_on_main_thread(move || update_layout(|l| Layout { bottom, ..l }));
    Ok(())
}

/// Move the pane to the next monitor.
#[tauri::command]
fn next_screen(app: AppHandle) {
    let _ = app.run_on_main_thread(|| {
        PANE.with(|p| {
            let Some(w) = p.borrow().clone() else { return };
            let Some(gw) = w.window() else { return };
            let display = gw.display();
            let n = display.n_monitors();
            if n < 2 {
                return;
            }
            let current = display.monitor_at_window(&gw);
            let i = (0..n).find(|&i| display.monitor(i) == current).unwrap_or(0);
            if let Some(m) = display.monitor((i + 1) % n) {
                w.set_monitor(&m);
            }
        });
    });
}

#[tauri::command]
fn set_expanded(app: AppHandle, expanded: bool) {
    let _ = app.run_on_main_thread(move || update_layout(|l| Layout { expanded, ..l }));
}

/// Colors of the current Aura OS theme (~/.config/aura/themes/<name>.conf).
#[tauri::command]
fn theme() -> Result<HashMap<String, String>, String> {
    let dir = config_dir().ok_or("no config dir")?.join("aura");
    let name = fs::read_to_string(dir.join("theme")).map_err(|e| format!("no current theme: {e}"))?;
    let name = name.trim();
    if !valid_name(name) {
        return Err(format!("bad theme name {name:?}"));
    }
    let text = fs::read_to_string(dir.join("themes").join(format!("{name}.conf")))
        .map_err(|e| format!("can't read theme {name}: {e}"))?;
    Ok(parse_kv(&text))
}

#[tauri::command]
async fn apps() -> Vec<AppEntry> {
    scan_apps()
}

/// Start an app by its .desktop id; only ids from the scanned list are accepted.
#[tauri::command]
async fn launch(id: String) -> Result<(), String> {
    if !scan_apps().iter().any(|a| a.id == id) {
        return Err(format!("No app called {id:?}"));
    }
    let mut child = Command::new("gtk-launch")
        .arg(&id)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Couldn't start {id}: {e}"))?;
    // Reap gtk-launch so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[tauri::command]
async fn health_check(force: bool) -> Result<health::Sample, String> {
    tauri::async_runtime::spawn_blocking(move || health::check(force))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn health_history(days: u64) -> Vec<health::Sample> {
    health::history(days.clamp(1, HISTORY_DAYS_MAX))
}

/// Who Aura is in the pane and what it can do, plus the latest PC health, so the
/// model answers as Aura about this machine instead of as a generic chatbot.
fn system_prompt(h: &health::Sample) -> String {
    let issues = if h.issues.is_empty() { "none".to_string() } else { h.issues.join("; ") };
    format!(
        "You are Aura, the assistant living in the Aura pane on Dule's Aura OS desktop (Arch Linux + sway). \
Answer briefly and plainly; Dule is not a developer. \
What the pane can do: typing an app's name and pressing Enter starts that app; typing \"system\" opens \
the PC health graph (health %, CPU, memory, temperature over the last 7 days, plus the issue list). \
When Dule asks about the computer's state or a graph, the pane opens that graph for him; summarise the numbers \
below in one or two sentences and point at the graph. Your agent tools (shell, files) are offline right now; say so instead \
of inventing results. \
Current PC health: {}% (CPU {:.0}%, memory {:.0}%, swap {:.0}%, fullest disk {:.0}%, hottest sensor {:.0}°C, \
{} journal errors in the last hour). Issues: {}.",
        h.score, h.cpu, h.mem, h.swap, h.disk, h.temp, h.errors, issues
    )
}

/// aura-serve's agent WebSocket for an engine endpoint: same host, ws scheme.
fn agent_ws_url(endpoint: &str, token: &str) -> String {
    let origin = endpoint.split("/v1/").next().unwrap_or(endpoint).replacen("http", "ws", 1);
    format!("{origin}/?token={token}")
}

/// Where the pane's JS reaches the full aura-code agent (tools, shell, files).
#[tauri::command]
fn engine_ws() -> Result<String, String> {
    let endpoint = std::env::var("AURA_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ENGINE_URL.into());
    Ok(agent_ws_url(&endpoint, &engine_token()?))
}

/// Fallback when the agent socket is down. Chat with aura-serve (OpenAI-style). Endpoint: $AURA_ENDPOINT or the local
/// default; token: $AURA_TOKEN or ~/.aura/server-token, read on every call.
#[tauri::command]
async fn ask(app: AppHandle, messages: Vec<ChatMsg>) -> Result<String, String> {
    let msgs: Vec<Value> = messages
        .iter()
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant") && !m.content.trim().is_empty())
        .map(|m| json!({ "role": m.role, "content": m.content }))
        .collect();
    let msgs = msgs[msgs.len().saturating_sub(HISTORY_MAX)..].to_vec();
    if msgs.last().map(|m| m["role"] != "user").unwrap_or(true) {
        return Err("Nothing to send.".into());
    }

    let h = tauri::async_runtime::spawn_blocking(|| health::check(false)).await.unwrap_or_default();
    let msgs: Vec<Value> = std::iter::once(json!({ "role": "system", "content": system_prompt(&h) }))
        .chain(msgs)
        .collect();

    let url = std::env::var("AURA_ENDPOINT")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ENGINE_URL.into());
    let token = engine_token()?;
    let client = reqwest::Client::builder()
        .timeout(ENGINE_TIMEOUT)
        .build()
        .map_err(|e| e.to_string())?;
    let res = client
        .post(&url)
        .bearer_auth(token)
        .json(&json!({ "messages": msgs }))
        .send()
        .await
        .map_err(|e| {
            if e.is_connect() {
                format!("Engine not reachable at {url} — is aura-serve running?")
            } else if e.is_timeout() {
                "Engine took too long to answer.".to_string()
            } else {
                e.to_string()
            }
        })?;
    let status = res.status();
    let body: Value = res
        .json()
        .await
        .map_err(|e| format!("Engine sent a non-JSON reply ({status}): {e}"))?;
    let reply = parse_reply(&body)?;
    after_task(app);
    Ok(reply)
}

// ---------- main ----------

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if argv.iter().any(|a| a == "--focus") {
                focus_input(app);
            }
        }))
        .manage(Recent(Mutex::new(Vec::new())))
        .invoke_handler(tauri::generate_handler![
            ask,
            engine_ws,
            recent,
            focus_window,
            grab_focus,
            release_focus,
            set_mode,
            set_shown,
            set_edge,
            set_expanded,
            next_screen,
            theme,
            apps,
            launch,
            health_check,
            health_history
        ])
        .setup(|app| {
            let win = app.get_webview_window("main").ok_or("no main window")?;
            setup_layer_shell(&win)?;
            seed_recent(app.handle());
            watch_sway(app.handle().clone());
            watch_health(app.handle().clone());
            audio::watch(app.handle().clone());
            if std::env::args().any(|a| a == "--focus") {
                focus_input(app.handle());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running aura-pane");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_at_prefers_floating_and_skips_hidden() {
        let tree = json!({"nodes": [{"nodes": [
            {"id": 1, "pid": 10, "visible": true, "rect": {"x": 0, "y": 0, "width": 960, "height": 1080}},
            {"id": 2, "pid": 11, "visible": false, "rect": {"x": 960, "y": 0, "width": 960, "height": 1080}}
        ], "floating_nodes": [
            {"id": 3, "pid": 12, "visible": true, "rect": {"x": 100, "y": 100, "width": 200, "height": 200}}
        ]}]});
        assert_eq!(window_at(&tree, 50, 50), Some(1));
        assert_eq!(window_at(&tree, 150, 150), Some(3));
        assert_eq!(window_at(&tree, 1000, 50), None);
    }

    #[test]
    fn agent_ws_url_keeps_host_and_swaps_scheme() {
        assert_eq!(
            agent_ws_url("http://127.0.0.1:7337/v1/chat/completions", "ab12"),
            "ws://127.0.0.1:7337/?token=ab12"
        );
        assert_eq!(agent_ws_url("https://box:7337/v1/chat/completions", "t"), "wss://box:7337/?token=t");
    }

    #[test]
    fn parses_theme_conf() {
        let kv = parse_kv("# Aura theme\nNAME=\"ocean-depth\"\nBG=\"#030812\"\nFONT='JetBrains, mono'\nbad line\n");
        assert_eq!(kv["NAME"], "ocean-depth");
        assert_eq!(kv["BG"], "#030812");
        assert_eq!(kv["FONT"], "JetBrains, mono");
        assert_eq!(kv.len(), 3);
    }

    #[test]
    fn rejects_theme_names_with_paths() {
        assert!(valid_name("tokyo-night"));
        assert!(!valid_name("../../etc/passwd"));
        assert!(!valid_name(""));
    }

    #[test]
    fn reads_desktop_entries() {
        let app = "[Desktop Entry]\nType=Application\nName=Firefox\nName[sr]=Фајерфокс\n[Desktop Action new]\nName=New Window\n";
        assert_eq!(desktop_name(app).as_deref(), Some("Firefox"));
        assert_eq!(desktop_name("[Desktop Entry]\nType=Application\nName=X\nNoDisplay=true\n"), None);
        assert_eq!(desktop_name("[Desktop Entry]\nType=Link\nName=X\n"), None);
    }

    #[test]
    fn system_prompt_names_aura_and_carries_health() {
        let h = health::Sample { score: 63, disk: 91.0, issues: vec!["Disk / is 91% full".into()], ..Default::default() };
        let p = system_prompt(&h);
        assert!(p.starts_with("You are Aura"));
        assert!(p.contains("63%") && p.contains("Disk / is 91% full") && p.contains("\"system\""));
        assert!(system_prompt(&health::Sample::default()).contains("Issues: none."));
    }

    #[test]
    fn parses_engine_replies() {
        let ok = json!({ "choices": [{ "message": { "content": "Hello there friend." } }] });
        assert_eq!(parse_reply(&ok), Ok("Hello there friend.".into()));
        let err = json!({ "error": { "message": "bad token" } });
        assert_eq!(parse_reply(&err), Err("Engine: bad token".into()));
    }
}
