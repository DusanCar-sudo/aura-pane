// Aura pane — right-side layer-shell panel for Aura OS (sway / Wayland).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use gtk::glib;
use gtk::prelude::*;
use gtk_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};
use serde::Serialize;
use serde_json::Value;
use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager, WebviewWindow};

const PANE_WIDTH: i32 = 360;
const STRIP_WIDTH: i32 = 4;
const MARGIN: i32 = 10;
const RECENT_MAX: usize = 8;
const GRAB_MS: u64 = 250;

thread_local! {
    // GTK objects are !Send: they live on the main thread only.
    static PANE: RefCell<Option<gtk::ApplicationWindow>> = RefCell::new(None);
    static WEBVIEW: RefCell<Option<gtk::Widget>> = RefCell::new(None);
    // (auto_hide, shown)
    static VIS: Cell<(bool, bool)> = Cell::new((true, false));
}

#[derive(Clone, Serialize)]
struct RecentItem {
    id: i64,
    app: String,
    title: String,
}

struct Recent(Mutex<Vec<RecentItem>>);

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
/// Auto-hide: tiles keep the full screen minus a 4px strip; when shown,
/// the pane slides over the tiles without reflowing them.
fn apply_layout() {
    let (auto, shown) = VIS.with(|v| v.get());
    let open = !auto || shown;
    let width = if open { PANE_WIDTH } else { STRIP_WIDTH };
    let margin = if open { MARGIN } else { 0 };
    PANE.with(|p| {
        if let Some(w) = p.borrow().as_ref() {
            w.set_layer_shell_margin(Edge::Top, margin);
            w.set_layer_shell_margin(Edge::Bottom, margin);
            w.set_layer_shell_margin(Edge::Right, margin);
            if auto {
                w.set_exclusive_zone(STRIP_WIDTH);
            } else {
                w.auto_exclusive_zone_enable();
            }
            w.set_size_request(width, -1);
            w.resize(width, 1);
        }
    });
}

fn set_vis(auto: Option<bool>, shown: Option<bool>) {
    VIS.with(|v| {
        let (a, s) = v.get();
        v.set((auto.unwrap_or(a), shown.unwrap_or(s)));
    });
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
    pane.set_anchor(Edge::Top, true);
    pane.set_anchor(Edge::Bottom, true);
    pane.set_anchor(Edge::Right, true);
    pane.set_anchor(Edge::Left, false);
    pane.set_keyboard_mode(KeyboardMode::OnDemand);

    let mut webview = None;
    for child in old.children() {
        old.remove(&child);
        pane.add(&child);
        if webview.is_none() {
            webview = find_webview(&child);
        }
    }

    // Whenever sway hands the surface the keyboard, pass it to the webview.
    pane.connect_focus_in_event(|_, _| {
        focus_webview();
        glib::Propagation::Proceed
    });

    WEBVIEW.with(|v| *v.borrow_mut() = webview);
    PANE.with(|p| *p.borrow_mut() = Some(pane.clone()));
    apply_layout();

    pane.show_all();
    old.hide();
    Ok(())
}

/// Take the keyboard: exclusive just long enough for sway to move focus
/// to the pane, then back to on-demand so clicking a tile takes it back.
fn grab_keyboard(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| {
        PANE.with(|p| {
            if let Some(w) = p.borrow().as_ref() {
                w.set_keyboard_mode(KeyboardMode::Exclusive);
                w.present();
            }
        });
        focus_webview();
        glib::timeout_add_local_once(Duration::from_millis(GRAB_MS), || {
            PANE.with(|p| {
                if let Some(w) = p.borrow().as_ref() {
                    w.set_keyboard_mode(KeyboardMode::OnDemand);
                }
            });
            focus_webview();
        });
    });
}

fn focus_input(app: &AppHandle) {
    let _ = app.run_on_main_thread(|| set_vis(None, Some(true)));
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
        }
    });
}

// ---------- commands ----------

#[tauri::command]
fn recent(state: tauri::State<Recent>) -> Vec<RecentItem> {
    state.0.lock().unwrap().clone()
}

#[tauri::command]
fn focus_window(id: i64) -> Result<(), String> {
    Command::new("swaymsg")
        .arg(format!("[con_id={id}] focus"))
        .status()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
fn grab_focus(app: AppHandle) {
    grab_keyboard(&app);
}

/// Esc: hand the keyboard back to the most recent window.
#[tauri::command]
fn release_focus(state: tauri::State<Recent>) -> Result<(), String> {
    let last = state.0.lock().unwrap().first().map(|i| i.id);
    match last {
        Some(id) => focus_window(id),
        None => Ok(()),
    }
}

#[tauri::command]
fn set_mode(app: AppHandle, auto: bool) {
    let _ = app.run_on_main_thread(move || set_vis(Some(auto), Some(false)));
}

#[tauri::command]
fn set_shown(app: AppHandle, shown: bool) {
    let _ = app.run_on_main_thread(move || set_vis(None, Some(shown)));
}

#[tauri::command]
async fn ask(prompt: String) -> Result<String, String> {
    let Ok(url) = std::env::var("AURA_ENDPOINT") else {
        return Ok(format!(
            "Engine not connected — set AURA_ENDPOINT to your aura-code server. You said: \"{prompt}\""
        ));
    };
    let res = reqwest::Client::new()
        .post(&url)
        .json(&serde_json::json!({ "message": prompt }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let text = res.text().await.map_err(|e| e.to_string())?;
    if let Ok(v) = serde_json::from_str::<Value>(&text) {
        if let Some(reply) = v["reply"].as_str() {
            return Ok(reply.to_string());
        }
    }
    Ok(text)
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
            recent,
            focus_window,
            grab_focus,
            release_focus,
            set_mode,
            set_shown
        ])
        .setup(|app| {
            let win = app.get_webview_window("main").ok_or("no main window")?;
            setup_layer_shell(&win)?;
            seed_recent(app.handle());
            watch_sway(app.handle().clone());
            if std::env::args().any(|a| a == "--focus") {
                focus_input(app.handle());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running aura-pane");
}
