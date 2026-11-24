//! Non-activating 3-edge Tactile Whisper-Bar micro-launcher (`Bottom`, `Left`, `Right`).
//!
//! Architecture ("Edge Dock v2.1"):
//! - **Flush-to-Edge Zero-Dead-Zone NSPanel (`0pt` margin):** A single fixed `264×264pt`
//!   non-activating `NSPanel` sits flush against the active `work_area` edge (`Y + H = 1117`
//!   on an auto-hidden MacBook Dock, `X = 0` on `Left`, `X + W = 1728` on `Right`). The
//!   `46×9px` tactile capsule floats visually `7px` off the edge inside a `76×24px` flush
//!   hit zone, eliminating any dead pixel gap behind the bar.
//! - **Fitts's Law Infinite-Edge Backstop + Instant Hover:** A `20ms` (`50Hz`) CoreGraphics
//!   `CGEventGetLocation` tracker (`0` `enigo` log spam, `0` allocations) flips
//!   `ignore_cursor = false` inside a `112×36pt` (`+16pt` outer backstop) approach halo
//!   and triggers expansion immediately (`0ms` dwell) anywhere on, around, or *behind*
//!   the pill (`76×24pt` + `16pt` outer backstop), while keeping `ignore_cursor = true`
//!   everywhere else so clicks in editors/composers pass straight through.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use lowyap_ports::OverlayProjection;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri::{AppHandle, Emitter, LogicalPosition, LogicalSize, Manager, Position, Size};

#[cfg(not(target_os = "macos"))]
use tauri::WebviewWindowBuilder;

#[cfg(target_os = "macos")]
use tauri::WebviewUrl;
#[cfg(target_os = "macos")]
use tauri_nspanel::{
    tauri_panel, CollectionBehavior, ManagerExt, PanelBuilder, PanelLevel, StyleMask,
};

use crate::flow::settings::{self as flow_settings, EdgeDockPosition, FlowSettings};
use crate::settings;

#[cfg(target_os = "macos")]
tauri_panel! {
    panel!(EdgeDockPanel {
        config: {
            can_become_key_window: false,
            is_floating_panel: true
        }
    })
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy)]
struct CGPointFfi {
    x: f64,
    y: f64,
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn CGEventSourceButtonState(state_id: u32, button: u32) -> bool;
    fn CGEventCreate(source: *const std::ffi::c_void) -> *const std::ffi::c_void;
    fn CGEventGetLocation(event: *const std::ffi::c_void) -> CGPointFfi;
}

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRelease(cf: *const std::ffi::c_void);
}

/// Returns `true` when the primary (left) mouse button is physically pressed anywhere on macOS.
#[cfg(target_os = "macos")]
fn is_primary_mouse_button_down() -> bool {
    // 0 = kCGEventSourceStateCombinedSessionState, 0 = kCGMouseButtonLeft
    unsafe { CGEventSourceButtonState(0, 0) }
}

#[cfg(not(target_os = "macos"))]
fn is_primary_mouse_button_down() -> bool {
    false
}

/// Direct CoreGraphics global logical cursor position (`0` allocations, `0` `enigo` log spam).
#[cfg(target_os = "macos")]
pub(crate) fn native_cursor_location(_app_handle: &AppHandle) -> Option<(f64, f64)> {
    unsafe {
        let ev = CGEventCreate(std::ptr::null());
        if ev.is_null() {
            return None;
        }
        let pt = CGEventGetLocation(ev);
        CFRelease(ev);
        if pt.x.is_finite() && pt.y.is_finite() {
            Some((pt.x, pt.y))
        } else {
            None
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn native_cursor_location(app_handle: &AppHandle) -> Option<(f64, f64)> {
    crate::input::get_cursor_position(app_handle).map(|(x, y)| (x as f64, y as f64))
}

const DOCK_WINDOW_LABEL: &str = "edge_dock";

// Fixed logical canvas for all docked states (`Collapsed`, `Expanded`, `Popover`).
// Flush `0.0` outer edge margin guarantees zero dead-zone pixels behind the pill.
const DOCK_CANVAS_W: f64 = 264.0;
const DOCK_CANVAS_H: f64 = 264.0;
const BOTTOM_EDGE_MARGIN: f64 = 0.0;
const SIDE_EDGE_MARGIN: f64 = 0.0;
const EDGE_BACKSTOP_OVERSHOOT: f64 = 16.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EdgeDockMode {
    Collapsed,
    Expanded,
    Dragging,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct WorkAreaRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct HitRect {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl HitRect {
    fn contains(self, px: f64, py: f64) -> bool {
        px >= self.x && px <= (self.x + self.w) && py >= self.y && py <= (self.y + self.h)
    }
}

#[derive(Debug, Clone)]
struct RuntimeState {
    mode: EdgeDockMode,
    menu_open: bool,
    snooze_until_ms: Option<u64>,
    preview_position: EdgeDockPosition,
    cursor_rel_x: Option<f64>,
    cursor_rel_y: Option<f64>,
    active_work_area: Option<WorkAreaRect>,
    work_area_dirty: bool,
    rearm_dwell_on_exit: bool,
    applied_rect: Option<(f64, f64, f64, f64)>,
    drag_epoch: u64,
}

impl Default for RuntimeState {
    fn default() -> Self {
        Self {
            mode: EdgeDockMode::Collapsed,
            menu_open: false,
            snooze_until_ms: None,
            preview_position: EdgeDockPosition::Bottom,
            cursor_rel_x: None,
            cursor_rel_y: None,
            active_work_area: None,
            work_area_dirty: true,
            rearm_dwell_on_exit: false,
            applied_rect: None,
            drag_epoch: 0,
        }
    }
}

/// Strictly encapsulated leaf-lock store for `RuntimeState`.
///
/// Keeping `STATE` private inside this module and never passing `&AppHandle`
/// into `mutate` guarantees that no caller can ever re-enter `STATE` (self-deadlock)
/// or invoke Tauri/AppKit main-thread APIs while holding the mutex (`AB-BA` deadlock).
mod state_store {
    use super::RuntimeState;
    use once_cell::sync::Lazy;
    use std::sync::{Mutex, PoisonError};

    static STATE: Lazy<Mutex<RuntimeState>> = Lazy::new(|| Mutex::new(RuntimeState::default()));

    pub(super) fn snapshot() -> RuntimeState {
        STATE.lock().unwrap_or_else(PoisonError::into_inner).clone()
    }

    pub(super) fn mutate<R>(f: impl FnOnce(&mut RuntimeState) -> R) -> R {
        let mut guard = STATE.lock().unwrap_or_else(PoisonError::into_inner);
        f(&mut guard)
    }
}

static DOCK_VISIBLE: AtomicBool = AtomicBool::new(false);
static CURSOR_IGNORED: AtomicBool = AtomicBool::new(true);
static ORCHESTRATOR_STARTED: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct EdgeDockPresetItem {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
pub struct EdgeDockUiState {
    pub enabled: bool,
    pub snoozed: bool,
    pub position: EdgeDockPosition,
    pub mode: EdgeDockMode,
    pub menu_open: bool,
    pub preview_position: EdgeDockPosition,
    pub dictate_shortcut: String,
    pub meeting_shortcut: String,
    pub transform_shortcut: String,
    pub auto_apply_enabled: bool,
    pub selected_preset_id: String,
    pub active_preset_title: String,
    pub presets: Vec<EdgeDockPresetItem>,
    pub cursor_rel_x: Option<f64>,
    pub cursor_rel_y: Option<f64>,
    pub theme: String,
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_currently_snoozed(snooze_until_ms: Option<u64>) -> bool {
    snooze_until_ms.is_some_and(|until| now_epoch_ms() < until)
}

pub fn clear_edge_dock_snooze() {
    state_store::mutate(|runtime| {
        runtime.snooze_until_ms = None;
    });
}

fn format_shortcut_badge(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let mut parts = Vec::new();
    for token in trimmed.split('+') {
        let lower = token.trim().to_ascii_lowercase();
        let mapped = match lower.as_str() {
            "alt" | "option" => "⌥".to_string(),
            "cmd" | "command" | "super" | "meta" => "⌘".to_string(),
            "ctrl" | "control" => "⌃".to_string(),
            "shift" => "⇧".to_string(),
            "space" => "Space".to_string(),
            other => {
                let mut chars = other.chars();
                match chars.next() {
                    Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                    None => String::new(),
                }
            }
        };
        if !mapped.is_empty() {
            parts.push(mapped);
        }
    }
    parts.join("")
}

fn friendly_preset_name(id: &str, raw_name: &str) -> String {
    if id == lowyap_flow::BUILTIN_POLISH_PROMPT_ID
        && (raw_name == "Polish" || raw_name == "Improve Transcriptions")
    {
        "Polish".to_string()
    } else {
        raw_name.to_string()
    }
}

fn build_ui_state(app: &AppHandle) -> EdgeDockUiState {
    let flow = flow_settings::get(app);
    let app_settings = settings::get_settings(app);
    let runtime = state_store::snapshot();

    let dictate_shortcut = app_settings
        .bindings
        .get("transcribe")
        .map(|b| format_shortcut_badge(&b.current_binding))
        .unwrap_or_else(|| "⌥Space".to_string());
    let meeting_shortcut = app_settings
        .bindings
        .get("meeting_notes")
        .map(|b| format_shortcut_badge(&b.current_binding))
        .filter(|s| !s.is_empty())
        .unwrap_or_default();
    let transform_shortcut = app_settings
        .bindings
        .get("transform")
        .map(|b| format_shortcut_badge(&b.current_binding))
        .unwrap_or_else(|| "⌥1".to_string());

    let mut presets: Vec<EdgeDockPresetItem> = app_settings
        .post_process_prompts
        .iter()
        .map(|p| EdgeDockPresetItem {
            id: p.id.clone(),
            name: friendly_preset_name(&p.id, &p.name),
        })
        .collect();
    if presets.is_empty() {
        presets.push(EdgeDockPresetItem {
            id: lowyap_flow::BUILTIN_POLISH_PROMPT_ID.to_string(),
            name: "Polish".to_string(),
        });
        presets.push(EdgeDockPresetItem {
            id: lowyap_flow::BUILTIN_PROMPT_ENGINEER_ID.to_string(),
            name: "Prompt Engineer".to_string(),
        });
    }

    let selected_preset_id = app_settings
        .post_process_selected_prompt_id
        .clone()
        .filter(|id| presets.iter().any(|p| &p.id == id))
        .or_else(|| presets.first().map(|p| p.id.clone()))
        .unwrap_or_else(|| lowyap_flow::BUILTIN_POLISH_PROMPT_ID.to_string());

    let active_preset_title = presets
        .iter()
        .find(|p| p.id == selected_preset_id)
        .map(|p| p.name.clone())
        .unwrap_or_else(|| "Polish".to_string());

    let theme = format!("{:?}", app_settings.theme).to_ascii_lowercase();

    EdgeDockUiState {
        enabled: flow.edge_dock_enabled,
        snoozed: is_currently_snoozed(runtime.snooze_until_ms),
        position: flow.edge_dock_position,
        mode: runtime.mode,
        menu_open: runtime.menu_open,
        preview_position: runtime.preview_position,
        dictate_shortcut,
        meeting_shortcut,
        transform_shortcut,
        auto_apply_enabled: app_settings.post_process_enabled,
        selected_preset_id,
        active_preset_title,
        presets,
        cursor_rel_x: runtime.cursor_rel_x,
        cursor_rel_y: runtime.cursor_rel_y,
        theme,
    }
}

fn emit_dock_state(app: &AppHandle) {
    let payload = build_ui_state(app);
    let _ = app.emit("edge-dock-state", &payload);
}

fn get_monitor_with_cursor(app_handle: &AppHandle) -> Option<tauri::Monitor> {
    if let Some((mouse_x, mouse_y)) = native_cursor_location(app_handle) {
        if let Ok(monitors) = app_handle.available_monitors() {
            for monitor in monitors {
                let scale = monitor.scale_factor().max(0.5);
                let mx = monitor.position().x as f64 / scale;
                let my = monitor.position().y as f64 / scale;
                let mw = monitor.size().width as f64 / scale;
                let mh = monitor.size().height as f64 / scale;
                if mouse_x >= mx && mouse_x < mx + mw && mouse_y >= my && mouse_y < my + mh {
                    return Some(monitor);
                }
            }
        }
    }
    app_handle.primary_monitor().ok().flatten()
}

fn resolve_work_area(app_handle: &AppHandle) -> Option<WorkAreaRect> {
    let monitor = get_monitor_with_cursor(app_handle)?;
    let scale = monitor.scale_factor().max(0.5);
    #[cfg(target_os = "macos")]
    {
        let wa = monitor.work_area();
        Some(WorkAreaRect {
            x: wa.position.x as f64 / scale,
            y: wa.position.y as f64 / scale,
            width: wa.size.width as f64 / scale,
            height: wa.size.height as f64 / scale,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(WorkAreaRect {
            x: monitor.position().x as f64 / scale,
            y: monitor.position().y as f64 / scale,
            width: monitor.size().width as f64 / scale,
            height: monitor.size().height as f64 / scale - 40.0,
        })
    }
}

/// Computes exact logical `(x, y, width, height)` for the requested dock mode and edge.
/// `Collapsed` and `Expanded` share a single fixed `264×264` canvas so hover/popover
/// transitions never trigger an OS window resize.
fn dock_geometry(
    wa: WorkAreaRect,
    position: EdgeDockPosition,
    mode: EdgeDockMode,
) -> (f64, f64, f64, f64) {
    match mode {
        EdgeDockMode::Dragging => (wa.x, wa.y, wa.width, wa.height),
        EdgeDockMode::Collapsed | EdgeDockMode::Expanded => match position {
            EdgeDockPosition::Left => (
                (wa.x + SIDE_EDGE_MARGIN).round(),
                (wa.y + (wa.height - DOCK_CANVAS_H) / 2.0).round(),
                DOCK_CANVAS_W,
                DOCK_CANVAS_H,
            ),
            EdgeDockPosition::Right => (
                (wa.x + wa.width - DOCK_CANVAS_W - SIDE_EDGE_MARGIN).round(),
                (wa.y + (wa.height - DOCK_CANVAS_H) / 2.0).round(),
                DOCK_CANVAS_W,
                DOCK_CANVAS_H,
            ),
            EdgeDockPosition::Bottom => (
                (wa.x + (wa.width - DOCK_CANVAS_W) / 2.0).round(),
                (wa.y + wa.height - DOCK_CANVAS_H - BOTTOM_EDGE_MARGIN).round(),
                DOCK_CANVAS_W,
                DOCK_CANVAS_H,
            ),
        },
    }
}

/// Approach halo around the `46×9px` pill where `ignore_cursor` flips to `false` ahead of
/// pointer arrival, including `+16pt` outer backstop past the screen edge.
fn approach_rect(wx: f64, wy: f64, ww: f64, wh: f64, pos: EdgeDockPosition) -> HitRect {
    match pos {
        EdgeDockPosition::Bottom => HitRect {
            x: wx + (ww - 112.0) / 2.0,
            y: wy + wh - 36.0,
            w: 112.0,
            h: 36.0 + EDGE_BACKSTOP_OVERSHOOT,
        },
        EdgeDockPosition::Left => HitRect {
            x: wx - EDGE_BACKSTOP_OVERSHOOT,
            y: wy + (wh - 112.0) / 2.0,
            w: 36.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 112.0,
        },
        EdgeDockPosition::Right => HitRect {
            x: wx + ww - 36.0,
            y: wy + (wh - 112.0) / 2.0,
            w: 36.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 112.0,
        },
    }
}

/// Zero-dead-zone trigger rect covering in-front, on-pill (`46×9px`), behind-pill (`0..8px`),
/// and `+16pt` infinite-edge backstop so sweeping around or behind the bar never misses.
fn trigger_rect(wx: f64, wy: f64, ww: f64, wh: f64, pos: EdgeDockPosition) -> HitRect {
    match pos {
        EdgeDockPosition::Bottom => HitRect {
            x: wx + (ww - 76.0) / 2.0,
            y: wy + wh - 24.0,
            w: 76.0,
            h: 24.0 + EDGE_BACKSTOP_OVERSHOOT,
        },
        EdgeDockPosition::Left => HitRect {
            x: wx - EDGE_BACKSTOP_OVERSHOOT,
            y: wy + (wh - 76.0) / 2.0,
            w: 24.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 76.0,
        },
        EdgeDockPosition::Right => HitRect {
            x: wx + ww - 24.0,
            y: wy + (wh - 76.0) / 2.0,
            w: 24.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 76.0,
        },
    }
}

/// Keep-alive interactive zone once expanded (`menu_open == false` hugs the 3 pills + tooltip
/// + outer edge backstop; `menu_open == true` covers the inline Transform popover).
fn expanded_keep_alive_rect(
    wx: f64,
    wy: f64,
    ww: f64,
    wh: f64,
    pos: EdgeDockPosition,
    menu_open: bool,
) -> HitRect {
    if menu_open {
        return HitRect {
            x: wx - EDGE_BACKSTOP_OVERSHOOT,
            y: wy - EDGE_BACKSTOP_OVERSHOOT,
            w: ww + EDGE_BACKSTOP_OVERSHOOT * 2.0,
            h: wh + EDGE_BACKSTOP_OVERSHOOT * 2.0,
        };
    }
    match pos {
        EdgeDockPosition::Bottom => HitRect {
            x: wx + (ww - 192.0) / 2.0,
            y: wy + wh - 86.0,
            w: 192.0,
            h: 86.0 + EDGE_BACKSTOP_OVERSHOOT,
        },
        EdgeDockPosition::Left => HitRect {
            x: wx - EDGE_BACKSTOP_OVERSHOOT,
            y: wy + (wh - 168.0) / 2.0,
            w: 188.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 168.0,
        },
        EdgeDockPosition::Right => HitRect {
            x: wx + ww - 188.0,
            y: wy + (wh - 168.0) / 2.0,
            w: 188.0 + EDGE_BACKSTOP_OVERSHOOT,
            h: 168.0,
        },
    }
}

/// Pure magnetic snap calculation mapping cursor coordinates relative to `work_area`
/// onto the closest of the three dockable edges (`Left`, `Bottom`, `Right`).
pub fn nearest_edge_position(
    rel_x: f64,
    rel_y: f64,
    wa_width: f64,
    wa_height: f64,
) -> EdgeDockPosition {
    let w = wa_width.max(1.0);
    let h = wa_height.max(1.0);
    let nx = (rel_x / w).clamp(0.0, 1.0);
    let ny = (rel_y / h).clamp(0.0, 1.0);

    let dist_left = ((nx - 0.02).powi(2) + (ny - 0.50).powi(2)).sqrt();
    let dist_right = ((nx - 0.98).powi(2) + (ny - 0.50).powi(2)).sqrt();
    let dist_bottom = ((nx - 0.50).powi(2) * 0.65 + (ny - 0.96).powi(2)).sqrt();

    if dist_bottom <= dist_left && dist_bottom <= dist_right {
        EdgeDockPosition::Bottom
    } else if dist_right < dist_left {
        EdgeDockPosition::Right
    } else {
        EdgeDockPosition::Left
    }
}

fn should_show_edge_dock(app: &AppHandle) -> bool {
    let flow = flow_settings::get(app);
    if !flow.edge_dock_enabled {
        return false;
    }
    if is_currently_snoozed(state_store::snapshot().snooze_until_ms) {
        return false;
    }
    let settings = settings::get_settings(app);
    let dictation_enabled = settings.overlay_style != settings::OverlayStyle::None;
    let active_overlay = crate::session_bus::bus().native_overlay(dictation_enabled);
    matches!(active_overlay, OverlayProjection::Hidden)
}

fn set_ignore_cursor(app: &AppHandle, ignore: bool) {
    if CURSOR_IGNORED.swap(ignore, Ordering::AcqRel) == ignore {
        return;
    }
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(window) = handle.get_webview_window(DOCK_WINDOW_LABEL) {
            let _ = window.set_ignore_cursor_events(ignore);
        }
    });
}

fn apply_window_geometry_and_visibility(app: &AppHandle) {
    let handle = app.clone();
    let _ = app.run_on_main_thread(move || {
        let Some(window) = handle.get_webview_window(DOCK_WINDOW_LABEL) else {
            return;
        };
        let show = should_show_edge_dock(&handle);
        if !show {
            if DOCK_VISIBLE.swap(false, Ordering::AcqRel) {
                CURSOR_IGNORED.store(true, Ordering::Release);
                let _ = window.set_ignore_cursor_events(true);
                #[cfg(target_os = "macos")]
                if let Ok(panel) = handle.get_webview_panel(DOCK_WINDOW_LABEL) {
                    panel.hide();
                    return;
                }
                let _ = window.hide();
            }
            return;
        }

        let flow = flow_settings::get(&handle);
        let snap = state_store::snapshot();
        let resolved_wa = if snap.work_area_dirty || snap.active_work_area.is_none() {
            resolve_work_area(&handle).or(snap.active_work_area)
        } else {
            snap.active_work_area
        };
        let Some(wa) = resolved_wa else {
            return;
        };
        let mode = snap.mode;
        let rect = dock_geometry(wa, flow.edge_dock_position, mode);
        let rect_changed = snap.applied_rect.map_or(true, |prev| {
            (prev.0 - rect.0).abs() > 0.5
                || (prev.1 - rect.1).abs() > 0.5
                || (prev.2 - rect.2).abs() > 0.5
                || (prev.3 - rect.3).abs() > 0.5
        });

        state_store::mutate(|runtime| {
            runtime.active_work_area = Some(wa);
            runtime.work_area_dirty = false;
            if rect_changed {
                runtime.applied_rect = Some(rect);
            }
        });

        if rect_changed {
            let (x, y, width, height) = rect;
            let _ = window.set_size(Size::Logical(LogicalSize { width, height }));
            let _ = window.set_position(Position::Logical(LogicalPosition { x, y }));
        }

        if mode == EdgeDockMode::Dragging {
            CURSOR_IGNORED.store(false, Ordering::Release);
            let _ = window.set_ignore_cursor_events(false);
        }

        if !DOCK_VISIBLE.swap(true, Ordering::AcqRel) {
            // Default to click-through until the cursor enters the approach halo.
            if mode == EdgeDockMode::Collapsed {
                CURSOR_IGNORED.store(true, Ordering::Release);
                let _ = window.set_ignore_cursor_events(true);
            }
            #[cfg(target_os = "macos")]
            if let Ok(panel) = handle.get_webview_panel(DOCK_WINDOW_LABEL) {
                panel.order_front_regardless();
                return;
            }
            let _ = window.show();
        }
    });
}

fn ensure_v2_bottom_default_seeded(app_handle: &AppHandle) -> FlowSettings {
    let mut flow = flow_settings::get(app_handle);
    if !flow.edge_dock_v2_seeded {
        flow.edge_dock_v2_seeded = true;
        if flow.edge_dock_position == EdgeDockPosition::Left {
            flow.edge_dock_position = EdgeDockPosition::Bottom;
        }
        if let Ok(saved) = flow_settings::set(app_handle, flow.clone()) {
            return saved;
        }
    }
    flow
}

#[derive(Debug, Clone, Copy, Serialize)]
struct EdgeDockCursorPayload {
    x: f64,
    y: f64,
}

fn spawn_cursor_orchestrator(app: AppHandle) {
    if ORCHESTRATOR_STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        let mut outside_ticks: u8 = 0;
        let mut display_check_ticks: u8 = 0;
        let mut last_rel_cursor: Option<(f64, f64)> = None;

        loop {
            if !DOCK_VISIBLE.load(Ordering::Acquire) {
                outside_ticks = 0;
                display_check_ticks = 0;
                // Wake automatically if a temporary snooze just expired.
                let now_ms = now_epoch_ms();
                let snooze_expired = state_store::mutate(|runtime| {
                    if let Some(until) = runtime.snooze_until_ms {
                        if now_ms >= until {
                            runtime.snooze_until_ms = None;
                            return true;
                        }
                    }
                    false
                });
                if snooze_expired {
                    sync_edge_dock_visibility(&app);
                }
                tokio::time::sleep(Duration::from_millis(180)).await;
                continue;
            }

            tokio::time::sleep(Duration::from_millis(20)).await;

            let snap = state_store::snapshot();
            let (mode, menu_open, wa, rearm_dwell) = (
                snap.mode,
                snap.menu_open,
                snap.active_work_area,
                snap.rearm_dwell_on_exit,
            );

            if mode == EdgeDockMode::Dragging {
                outside_ticks = 0;
                set_ignore_cursor(&app, false);
                continue;
            }

            // Periodically re-check NSScreen work_area when idle so display hot-plug,
            // resolution changes, or macOS Dock auto-hide toggles self-heal automatically.
            if mode == EdgeDockMode::Collapsed {
                display_check_ticks = display_check_ticks.wrapping_add(1);
                if display_check_ticks >= 125 {
                    display_check_ticks = 0;
                    state_store::mutate(|rt| {
                        rt.work_area_dirty = true;
                    });
                    apply_window_geometry_and_visibility(&app);
                }
            } else {
                display_check_ticks = 0;
            }

            let Some(wa) = wa else {
                continue;
            };
            let Some((cx, cy)) = native_cursor_location(&app) else {
                continue;
            };

            let pos = flow_settings::get(&app).edge_dock_position;
            let (wx, wy, ww, wh) = dock_geometry(wa, pos, mode);

            match mode {
                EdgeDockMode::Collapsed => {
                    outside_ticks = 0;
                    last_rel_cursor = None;
                    let in_approach = approach_rect(wx, wy, ww, wh, pos).contains(cx, cy);
                    set_ignore_cursor(&app, !in_approach);

                    let in_trigger = trigger_rect(wx, wy, ww, wh, pos).contains(cx, cy);
                    if !in_trigger && rearm_dwell {
                        state_store::mutate(|rt| {
                            rt.rearm_dwell_on_exit = false;
                        });
                    } else if in_trigger && !rearm_dwell && !is_primary_mouse_button_down() {
                        set_ignore_cursor(&app, false);
                        set_edge_dock_expanded(app.clone(), true);
                    }
                }
                EdgeDockMode::Expanded => {
                    let keep_alive =
                        expanded_keep_alive_rect(wx, wy, ww, wh, pos, menu_open).contains(cx, cy);
                    // Only intercept clicks inside the active controls/popover region so underlying
                    // editors/composers remain clickable everywhere else in the 264×264 canvas.
                    set_ignore_cursor(&app, !keep_alive);

                    // Stream window-relative cursor coordinates to the unfocused NSPanel webview
                    // whenever the pointer moves across the 3 pills so hover & tooltip tracking
                    // remain 100% responsive even while another app owns key focus.
                    if keep_alive && !menu_open {
                        let rel = (cx - wx, cy - wy);
                        let moved = last_rel_cursor.is_none_or(|(px, py)| {
                            (rel.0 - px).abs() >= 0.75 || (rel.1 - py).abs() >= 0.75
                        });
                        if moved {
                            last_rel_cursor = Some(rel);
                            let _ = app.emit_to(
                                DOCK_WINDOW_LABEL,
                                "edge-dock-cursor",
                                EdgeDockCursorPayload { x: rel.0, y: rel.1 },
                            );
                        }
                    } else if last_rel_cursor.is_some() {
                        last_rel_cursor = None;
                        let _ = app.emit_to(
                            DOCK_WINDOW_LABEL,
                            "edge-dock-cursor",
                            EdgeDockCursorPayload {
                                x: -1000.0,
                                y: -1000.0,
                            },
                        );
                    }

                    if menu_open && !keep_alive && is_primary_mouse_button_down() {
                        outside_ticks = 0;
                        set_edge_dock_expanded(app.clone(), false);
                    } else if keep_alive || is_primary_mouse_button_down() {
                        outside_ticks = 0;
                    } else if !menu_open {
                        outside_ticks = outside_ticks.saturating_add(1);
                        if outside_ticks >= 3 {
                            outside_ticks = 0;
                            set_edge_dock_expanded(app.clone(), false);
                        }
                    }
                }
                EdgeDockMode::Dragging => {}
            }
        }
    });
}

/// Creates the non-activating `edge_dock` panel at startup and syncs initial visibility.
#[cfg(target_os = "macos")]
pub fn create_edge_dock(app_handle: &AppHandle) {
    let flow = ensure_v2_bottom_default_seeded(app_handle);
    let Some(wa) = resolve_work_area(app_handle) else {
        log::warn!("edge_dock: no monitor work area found; skipping panel creation");
        return;
    };
    let rect = dock_geometry(wa, flow.edge_dock_position, EdgeDockMode::Collapsed);
    state_store::mutate(|runtime| {
        runtime.preview_position = flow.edge_dock_position;
        runtime.active_work_area = Some(wa);
        runtime.work_area_dirty = false;
        runtime.applied_rect = Some(rect);
    });
    let (x, y, width, height) = rect;

    match PanelBuilder::<_, EdgeDockPanel>::new(app_handle, DOCK_WINDOW_LABEL)
        .url(WebviewUrl::App("src/dock/index.html".into()))
        .title("lowyap Dock")
        .position(Position::Logical(LogicalPosition { x, y }))
        .level(PanelLevel::Status)
        .size(Size::Logical(LogicalSize { width, height }))
        .has_shadow(false)
        .transparent(true)
        .no_activate(true)
        .corner_radius(0.0)
        .style_mask(StyleMask::empty().borderless().nonactivating_panel())
        .with_window(|w| {
            w.decorations(false)
                .transparent(true)
                .focusable(false)
                .accept_first_mouse(true)
        })
        .collection_behavior(
            CollectionBehavior::new()
                .can_join_all_spaces()
                .full_screen_auxiliary(),
        )
        .build()
    {
        Ok(panel) => {
            if let Some(window) = app_handle.get_webview_window(DOCK_WINDOW_LABEL) {
                CURSOR_IGNORED.store(true, Ordering::Release);
                let _ = window.set_ignore_cursor_events(true);
            }
            if should_show_edge_dock(app_handle) {
                DOCK_VISIBLE.store(true, Ordering::Release);
                panel.order_front_regardless();
            } else {
                DOCK_VISIBLE.store(false, Ordering::Release);
                panel.hide();
            }
            spawn_cursor_orchestrator(app_handle.clone());
        }
        Err(err) => {
            log::error!("Failed to create edge_dock panel: {}", err);
        }
    }
    let _ = ManagerExt::get_webview_panel(app_handle, DOCK_WINDOW_LABEL);
}

#[cfg(not(target_os = "macos"))]
pub fn create_edge_dock(app_handle: &AppHandle) {
    let flow = ensure_v2_bottom_default_seeded(app_handle);
    let Some(wa) = resolve_work_area(app_handle) else {
        return;
    };
    let (x, y, width, height) = dock_geometry(wa, flow.edge_dock_position, EdgeDockMode::Collapsed);
    let builder = WebviewWindowBuilder::new(
        app_handle,
        DOCK_WINDOW_LABEL,
        tauri::WebviewUrl::App("src/dock/index.html".into()),
    )
    .title("lowyap Dock")
    .resizable(false)
    .inner_size(width, height)
    .position(x, y)
    .shadow(false)
    .maximizable(false)
    .minimizable(false)
    .closable(false)
    .accept_first_mouse(true)
    .decorations(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .transparent(true)
    .focusable(false)
    .focused(false)
    .visible(should_show_edge_dock(app_handle));

    if builder.build().is_ok() {
        spawn_cursor_orchestrator(app_handle.clone());
    }
}

/// Pure state transition for `sync_edge_dock_visibility` — kept free of `AppHandle`
/// so visibility syncs can never re-enter `STATE` or dispatch to the main thread under lock.
fn apply_visibility_sync_to_runtime(
    runtime: &mut RuntimeState,
    show: bool,
    position: EdgeDockPosition,
) {
    if !show && runtime.mode != EdgeDockMode::Collapsed {
        runtime.mode = EdgeDockMode::Collapsed;
        runtime.menu_open = false;
        runtime.cursor_rel_x = None;
        runtime.cursor_rel_y = None;
    }
    if runtime.mode == EdgeDockMode::Collapsed {
        runtime.preview_position = position;
        if show {
            runtime.work_area_dirty = true;
        }
    }
}

/// Syncs `edge_dock` visibility and position whenever `SessionBus` or `FlowSettings` changes.
pub fn sync_edge_dock_visibility(app: &AppHandle) {
    let show = should_show_edge_dock(app);
    let position = flow_settings::get(app).edge_dock_position;
    state_store::mutate(|runtime| {
        apply_visibility_sync_to_runtime(runtime, show, position);
    });
    apply_window_geometry_and_visibility(app);
    emit_dock_state(app);
}

fn spawn_drag_mouse_watchdog(app: AppHandle, epoch: u64) {
    tauri::async_runtime::spawn(async move {
        // Allow 60ms for drag gesture initiation.
        tokio::time::sleep(Duration::from_millis(60)).await;
        let mut ticks = 0u32;
        loop {
            tokio::time::sleep(Duration::from_millis(24)).await;
            ticks += 1;
            let snap = state_store::snapshot();
            if snap.mode != EdgeDockMode::Dragging || snap.drag_epoch != epoch {
                return;
            }
            let Some(wa) = snap.active_work_area else {
                let _ = finish_edge_dock_drag(app, None);
                return;
            };

            let mut snapped = None;
            if let Some((cx, cy)) = native_cursor_location(&app) {
                let rel_x = (cx - wa.x).clamp(0.0, wa.width);
                let rel_y = (cy - wa.y).clamp(0.0, wa.height);
                let nearest = nearest_edge_position(rel_x, rel_y, wa.width, wa.height);
                snapped = Some(nearest);
                let changed = state_store::mutate(|runtime| {
                    if runtime.mode == EdgeDockMode::Dragging && runtime.drag_epoch == epoch {
                        if runtime.preview_position != nearest
                            || runtime
                                .cursor_rel_x
                                .map_or(true, |prev| (prev - rel_x).abs() > 2.0)
                            || runtime
                                .cursor_rel_y
                                .map_or(true, |prev| (prev - rel_y).abs() > 2.0)
                        {
                            runtime.preview_position = nearest;
                            runtime.cursor_rel_x = Some(rel_x);
                            runtime.cursor_rel_y = Some(rel_y);
                            return true;
                        }
                    }
                    false
                });
                if changed {
                    emit_dock_state(&app);
                }
            }

            // Finish immediately when primary mouse button is released OR after 12s safety cap.
            if !is_primary_mouse_button_down() || ticks > 500 {
                let _ = finish_edge_dock_drag(app, snapped);
                return;
            }
        }
    });
}

#[tauri::command]
#[specta::specta]
pub fn get_edge_dock_state(app: AppHandle) -> EdgeDockUiState {
    build_ui_state(&app)
}

#[tauri::command]
#[specta::specta]
pub fn set_edge_dock_expanded(app: AppHandle, expanded: bool) {
    let cursor = if !expanded {
        native_cursor_location(&app)
    } else {
        None
    };
    let pos = flow_settings::get(&app).edge_dock_position;
    let mut aborted_drag = false;
    let changed = state_store::mutate(|runtime| {
        if runtime.mode == EdgeDockMode::Dragging {
            aborted_drag = true;
            return false;
        }
        let target = if expanded {
            EdgeDockMode::Expanded
        } else {
            EdgeDockMode::Collapsed
        };
        if runtime.mode == target && (expanded || !runtime.menu_open) {
            false
        } else {
            runtime.mode = target;
            if !expanded {
                runtime.menu_open = false;
                // Only latch re-entry suppression if the cursor is STILL parked inside `trigger_rect`
                // (e.g. Escape/click dismiss while hovering). Normal mouse-leave collapses stay ready!
                let parked_in_trigger = match (runtime.active_work_area, cursor) {
                    (Some(wa), Some((cx, cy))) => {
                        let (wx, wy, ww, wh) = dock_geometry(wa, pos, EdgeDockMode::Collapsed);
                        trigger_rect(wx, wy, ww, wh, pos).contains(cx, cy)
                    }
                    _ => false,
                };
                runtime.rearm_dwell_on_exit = parked_in_trigger;
            } else {
                runtime.rearm_dwell_on_exit = false;
                runtime.work_area_dirty = true;
            }
            true
        }
    });
    if aborted_drag {
        return;
    }
    if changed {
        set_ignore_cursor(&app, !expanded);
        apply_window_geometry_and_visibility(&app);
        emit_dock_state(&app);
    }
}

#[tauri::command]
#[specta::specta]
pub fn set_edge_dock_menu_open(app: AppHandle, open: bool) {
    let mut aborted_drag = false;
    let changed = state_store::mutate(|runtime| {
        if runtime.mode == EdgeDockMode::Dragging {
            aborted_drag = true;
            return false;
        }
        if open && runtime.mode == EdgeDockMode::Collapsed {
            runtime.mode = EdgeDockMode::Expanded;
        }
        if runtime.menu_open == open {
            false
        } else {
            runtime.menu_open = open;
            true
        }
    });
    if aborted_drag {
        return;
    }
    if changed {
        if open {
            set_ignore_cursor(&app, false);
        }
        emit_dock_state(&app);
    }
}

#[tauri::command]
#[specta::specta]
pub fn begin_edge_dock_drag(app: AppHandle) {
    let flow = flow_settings::get(&app);
    let cursor = native_cursor_location(&app);
    let fallback_wa = if state_store::snapshot().active_work_area.is_none() {
        resolve_work_area(&app)
    } else {
        None
    };
    let epoch = state_store::mutate(|runtime| {
        let wa = runtime.active_work_area.or(fallback_wa);
        runtime.active_work_area = wa;
        runtime.work_area_dirty = wa.is_none();
        runtime.mode = EdgeDockMode::Dragging;
        runtime.menu_open = false;
        runtime.preview_position = flow.edge_dock_position;
        if let (Some(wa), Some((cx, cy))) = (wa, cursor) {
            runtime.cursor_rel_x = Some((cx - wa.x).clamp(0.0, wa.width));
            runtime.cursor_rel_y = Some((cy - wa.y).clamp(0.0, wa.height));
        }
        runtime.drag_epoch = runtime.drag_epoch.wrapping_add(1);
        runtime.drag_epoch
    });
    set_ignore_cursor(&app, false);
    apply_window_geometry_and_visibility(&app);
    emit_dock_state(&app);
    spawn_drag_mouse_watchdog(app, epoch);
}

#[tauri::command]
#[specta::specta]
pub fn finish_edge_dock_drag(
    app: AppHandle,
    position: Option<EdgeDockPosition>,
) -> Result<EdgeDockPosition, String> {
    let maybe_chosen = state_store::mutate(|runtime| {
        if runtime.mode != EdgeDockMode::Dragging {
            return None;
        }
        let target = position.unwrap_or(runtime.preview_position);
        runtime.mode = EdgeDockMode::Collapsed;
        runtime.menu_open = false;
        runtime.preview_position = target;
        runtime.cursor_rel_x = None;
        runtime.cursor_rel_y = None;
        runtime.work_area_dirty = true;
        runtime.drag_epoch = runtime.drag_epoch.wrapping_add(1);
        Some(target)
    });
    let Some(chosen) = maybe_chosen else {
        return Ok(flow_settings::get(&app).edge_dock_position);
    };
    let mut flow = flow_settings::get(&app);
    if flow.edge_dock_position != chosen || !flow.edge_dock_v2_seeded {
        flow.edge_dock_position = chosen;
        flow.edge_dock_v2_seeded = true;
        let saved = flow_settings::set(&app, flow)?;
        let _ = app.emit("flow-settings-changed", &saved);
    }
    set_ignore_cursor(&app, true);
    apply_window_geometry_and_visibility(&app);
    emit_dock_state(&app);
    Ok(chosen)
}

#[tauri::command]
#[specta::specta]
pub fn set_edge_dock_position(
    app: AppHandle,
    position: EdgeDockPosition,
) -> Result<FlowSettings, String> {
    clear_edge_dock_snooze();
    let mut flow = flow_settings::get(&app);
    flow.edge_dock_position = position;
    flow.edge_dock_v2_seeded = true;
    let saved = flow_settings::set(&app, flow)?;
    state_store::mutate(|runtime| {
        runtime.preview_position = position;
        runtime.work_area_dirty = true;
    });
    let _ = app.emit("flow-settings-changed", &saved);
    apply_window_geometry_and_visibility(&app);
    emit_dock_state(&app);
    Ok(saved)
}

#[tauri::command]
#[specta::specta]
pub fn snooze_edge_dock(app: AppHandle, minutes: u32) {
    let clamped_mins = minutes.clamp(1, 24 * 60) as u64;
    let until = now_epoch_ms().saturating_add(clamped_mins * 60_000);
    state_store::mutate(|runtime| {
        runtime.snooze_until_ms = Some(until);
        runtime.mode = EdgeDockMode::Collapsed;
        runtime.menu_open = false;
    });
    sync_edge_dock_visibility(&app);
}

#[tauri::command]
#[specta::specta]
pub fn hide_edge_dock(app: AppHandle) -> Result<FlowSettings, String> {
    clear_edge_dock_snooze();
    let mut flow = flow_settings::get(&app);
    flow.edge_dock_enabled = false;
    flow.edge_dock_v2_seeded = true;
    let saved = flow_settings::set(&app, flow)?;
    state_store::mutate(|runtime| {
        runtime.mode = EdgeDockMode::Collapsed;
        runtime.menu_open = false;
    });
    let _ = app.emit("flow-settings-changed", &saved);
    sync_edge_dock_visibility(&app);
    Ok(saved)
}

#[tauri::command]
#[specta::specta]
pub fn trigger_dock_dictation(app: AppHandle) {
    set_edge_dock_expanded(app.clone(), false);
    crate::signal_handle::send_transcription_input(&app, "transcribe", "edge_dock");
}

#[tauri::command]
#[specta::specta]
pub fn trigger_dock_meeting(app: AppHandle) -> Result<(), String> {
    set_edge_dock_expanded(app.clone(), false);
    if !matches!(
        crate::session_bus::bus().snapshot().meeting,
        lowyap_ports::MeetingPhase::Idle
    ) {
        crate::session_bus::open_meetings_workspace(app);
        return Ok(());
    }
    crate::meeting::commands::start_meeting_notes(app, None, None, Some(false)).map(|_| ())
}

#[tauri::command(async)]
#[specta::specta]
pub async fn trigger_dock_transform(
    app: AppHandle,
    prompt_id: Option<String>,
) -> Result<crate::flow::TransformOutcome, String> {
    set_edge_dock_expanded(app.clone(), false);
    crate::flow::run_transform_action(app, prompt_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nearest_edge_snaps_cleanly_to_left_bottom_and_right() {
        let w = 1512.0;
        let h = 945.0;
        assert_eq!(
            nearest_edge_position(42.0, 470.0, w, h),
            EdgeDockPosition::Left
        );
        assert_eq!(
            nearest_edge_position(1470.0, 460.0, w, h),
            EdgeDockPosition::Right
        );
        assert_eq!(
            nearest_edge_position(756.0, 910.0, w, h),
            EdgeDockPosition::Bottom
        );
    }

    #[test]
    fn docked_canvas_never_resizes_between_collapsed_and_expanded() {
        let wa = WorkAreaRect {
            x: 0.0,
            y: 38.0,
            width: 1512.0,
            height: 907.0,
        };
        for edge in [
            EdgeDockPosition::Bottom,
            EdgeDockPosition::Left,
            EdgeDockPosition::Right,
        ] {
            let collapsed = dock_geometry(wa, edge, EdgeDockMode::Collapsed);
            let expanded = dock_geometry(wa, edge, EdgeDockMode::Expanded);
            assert_eq!(
                collapsed, expanded,
                "docked NSPanel must stay identical across hover states for {edge:?}"
            );
            assert_eq!(collapsed.2, DOCK_CANVAS_W);
            assert_eq!(collapsed.3, DOCK_CANVAS_H);
        }
    }

    #[test]
    fn approach_trigger_and_keep_alive_hit_rects_nest_and_cover_behind_bar_screen_edges() {
        // Real MacBook Pro 16" Retina logical geometry (`1728×1117`, auto-hidden macOS Dock).
        let wa = WorkAreaRect {
            x: 0.0,
            y: 33.0,
            width: 1728.0,
            height: 1084.0,
        };
        for edge in [
            EdgeDockPosition::Bottom,
            EdgeDockPosition::Left,
            EdgeDockPosition::Right,
        ] {
            let (wx, wy, ww, wh) = dock_geometry(wa, edge, EdgeDockMode::Collapsed);
            let trigger = trigger_rect(wx, wy, ww, wh, edge);
            let approach = approach_rect(wx, wy, ww, wh, edge);
            let cluster = expanded_keep_alive_rect(wx, wy, ww, wh, edge, false);
            let menu = expanded_keep_alive_rect(wx, wy, ww, wh, edge, true);

            // Trigger must sit strictly inside approach; approach inside cluster; cluster inside menu.
            assert!(
                trigger.x >= approach.x && (trigger.x + trigger.w) <= (approach.x + approach.w)
            );
            assert!(
                trigger.y >= approach.y && (trigger.y + trigger.h) <= (approach.y + approach.h)
            );
            assert!(
                approach.x >= cluster.x && (approach.x + approach.w) <= (cluster.x + cluster.w)
            );
            assert!(
                approach.y >= cluster.y && (approach.y + approach.h) <= (cluster.y + cluster.h)
            );
            assert!(cluster.x >= menu.x && (cluster.x + cluster.w) <= (menu.x + menu.w));
            assert!(cluster.y >= menu.y && (cluster.y + cluster.h) <= (menu.y + menu.h));
        }

        // Regression proof: sweeping around & behind the pill onto the physical screen edge
        // (`y = 1116.9` on Bottom, `x = 0.0` on Left, `x = 1727.9` on Right) hits BOTH the
        // NSPanel bounds AND `trigger_rect` + `expanded_keep_alive_rect` with zero dead pixels.
        let (bx, by, bw, bh) = dock_geometry(wa, EdgeDockPosition::Bottom, EdgeDockMode::Collapsed);
        assert_eq!(by + bh, 1117.0, "Bottom NSPanel must sit flush to y=1117");
        for behind_y in [1105.0, 1112.0, 1116.9, 1117.0] {
            assert!(
                trigger_rect(bx, by, bw, bh, EdgeDockPosition::Bottom).contains(864.0, behind_y),
                "Bottom trigger_rect missed behind-bar y={behind_y}"
            );
            assert!(
                expanded_keep_alive_rect(bx, by, bw, bh, EdgeDockPosition::Bottom, false)
                    .contains(864.0, behind_y),
                "Bottom keep_alive missed behind-bar y={behind_y}"
            );
        }

        let (lx, ly, lw, lh) = dock_geometry(wa, EdgeDockPosition::Left, EdgeDockMode::Collapsed);
        assert_eq!(lx, 0.0, "Left NSPanel must sit flush to x=0");
        for behind_x in [0.0, 2.0, 8.0] {
            assert!(trigger_rect(lx, ly, lw, lh, EdgeDockPosition::Left).contains(behind_x, 575.0));
        }

        let (rx, ry, rw, rh) = dock_geometry(wa, EdgeDockPosition::Right, EdgeDockMode::Collapsed);
        assert_eq!(rx + rw, 1728.0, "Right NSPanel must sit flush to x=1728");
        for behind_x in [1720.0, 1726.0, 1727.9, 1728.0] {
            assert!(
                trigger_rect(rx, ry, rw, rh, EdgeDockPosition::Right).contains(behind_x, 575.0)
            );
        }
    }

    #[test]
    fn visibility_sync_collapses_expanded_dock_and_never_deadlocks_under_contention() {
        let mut rt = RuntimeState {
            mode: EdgeDockMode::Expanded,
            menu_open: true,
            cursor_rel_x: Some(42.0),
            cursor_rel_y: Some(24.0),
            work_area_dirty: false,
            ..RuntimeState::default()
        };
        apply_visibility_sync_to_runtime(&mut rt, false, EdgeDockPosition::Right);
        assert_eq!(rt.mode, EdgeDockMode::Collapsed);
        assert!(!rt.menu_open);
        assert_eq!(rt.cursor_rel_x, None);
        assert_eq!(rt.preview_position, EdgeDockPosition::Right);

        apply_visibility_sync_to_runtime(&mut rt, true, EdgeDockPosition::Bottom);
        assert!(rt.work_area_dirty);
        assert_eq!(rt.preview_position, EdgeDockPosition::Bottom);

        // Hammer `state_store` across 4 threads interleaving snooze checks + visibility syncs
        // to prove reentrant / lock-inversion deadlocks are impossible.
        let mut handles = Vec::new();
        for t in 0..4u64 {
            handles.push(std::thread::spawn(move || {
                for i in 0..250u64 {
                    let snoozed = is_currently_snoozed(state_store::snapshot().snooze_until_ms);
                    state_store::mutate(|runtime| {
                        runtime.snooze_until_ms = if (t + i) % 3 == 0 {
                            Some(now_epoch_ms() + 60_000)
                        } else {
                            None
                        };
                        runtime.mode = if (t + i) % 2 == 0 {
                            EdgeDockMode::Expanded
                        } else {
                            EdgeDockMode::Collapsed
                        };
                        apply_visibility_sync_to_runtime(
                            runtime,
                            !snoozed,
                            EdgeDockPosition::Bottom,
                        );
                    });
                    clear_edge_dock_snooze();
                }
            }));
        }
        for h in handles {
            h.join().expect("state_store worker thread panicked");
        }
    }
}
