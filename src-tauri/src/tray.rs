//! System tray: the whole UI while no window is open.

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder, Wry};

use crate::tracker::machine::TrackerState;
use crate::tracker::runtime::TrackerStatus;

const TRAY_ID: &str = "downplayed";
const MAIN_WINDOW: &str = "main";

/// Menu items updated as the tracker state changes.
struct TrayItems {
    status: MenuItem<Wry>,
}

pub fn create(app: &AppHandle) -> tauri::Result<()> {
    let status = MenuItem::with_id(
        app,
        "status",
        status_text(&TrackerStatus::default()),
        false,
        None::<&str>,
    )?;
    let open = MenuItem::with_id(app, "open", "Open Downplayed", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let separator = PredefinedMenuItem::separator(app)?;
    let menu = Menu::with_items(app, &[&status, &separator, &open, &quit])?;

    let mut tray = TrayIconBuilder::with_id(TRAY_ID)
        .tooltip("Downplayed")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main_window(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    app.manage(TrayItems { status });
    Ok(())
}

/// Focuses the window, or creates it. Closing it destroys it (and its WebView2
/// processes); the app stays in the tray.
pub fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        return;
    }
    create_main_window(app, true);
}

/// Creates the window. Invisible windows exist for smoke tests (the page still loads).
pub fn create_main_window(app: &AppHandle, visible: bool) {
    let built = WebviewWindowBuilder::new(app, MAIN_WINDOW, WebviewUrl::App("index.html".into()))
        .title("Downplayed")
        .inner_size(960.0, 640.0)
        .min_inner_size(640.0, 420.0)
        .visible(visible)
        .build();
    if let Err(error) = built {
        tracing::error!(%error, "could not open the window");
    }
}

pub fn show_status(app: &AppHandle, status: &TrackerStatus) {
    let text = status_text(status);
    if let Some(tray) = app.tray_by_id(TRAY_ID) {
        let _ = tray.set_tooltip(Some(format!("Downplayed · {text}")));
    }
    if let Some(items) = app.try_state::<TrayItems>() {
        let _ = items.status.set_text(text);
    }
}

fn status_text(status: &TrackerStatus) -> String {
    match status.state {
        TrackerState::Idle if status.lcu_connected => "Idle · League client open".into(),
        TrackerState::Idle => "Idle · waiting for League".into(),
        TrackerState::Armed => "Champion select".into(),
        TrackerState::InGame => match &status.current {
            Some(current) if !current.tracking => "In game (not tracked)".into(),
            Some(current) if status.reconnecting => {
                format!("Reconnecting · {} moments", current.moments.len())
            }
            Some(current) => format!("In game · {} moments", current.moments.len()),
            None => "In game".into(),
        },
        TrackerState::Finalizing => "Match over · summarising".into(),
    }
}
