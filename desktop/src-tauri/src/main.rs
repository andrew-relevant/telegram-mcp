#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod account;
mod clients;

use std::sync::Arc;

use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{Manager, WindowEvent};
use telegram_mcp_ipc::app_data_dir;
use telegram_mcp_mcp::{serve_with, AppState, SendGate};

use account::{current_client_files, Desk};
use clients::ClientKind;

fn main() {
    let state = shared_state();
    let desk = Desk::start(
        Arc::clone(&state),
        current_client_files(),
        cfg!(target_os = "macos"),
    );
    tauri::async_runtime::spawn(async move {
        if let Err(error) = serve_with(state).await {
            eprintln!("{error}");
        }
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.show();
                let _ = window.unminimize();
                let _ = window.set_focus();
            }
        }))
        .manage(desk)
        .setup(|app| {
            if let Some(window) = app.get_webview_window("main") {
                let window = window.clone();
                window.clone().on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = window.hide();
                    }
                });
            }
            if let Err(error) = install_tray(app.handle()) {
                eprintln!("трей не создан: {error}");
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            status,
            save_credentials,
            submit_password,
            logout,
            set_sending,
            set_client
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn shared_state() -> Arc<AppState> {
    let gate = app_data_dir()
        .map(|dir| SendGate::load(&dir))
        .unwrap_or_else(|_| SendGate::off());
    AppState::new(gate)
}

fn install_tray(app: &tauri::AppHandle) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Открыть", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выйти", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &quit])?;
    let mut tray = TrayIconBuilder::new()
        .tooltip("Telegram MCP")
        .menu(&menu)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "show" => {
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let tauri::tray::TrayIconEvent::Click { .. } = event {
                let app = tray.app_handle();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                    let _ = window.unminimize();
                    let _ = window.set_focus();
                }
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[tauri::command]
fn status(desk: tauri::State<'_, Arc<Desk>>) -> account::Status {
    desk.status()
}

#[tauri::command]
async fn save_credentials(
    desk: tauri::State<'_, Arc<Desk>>,
    api_id: i32,
    api_hash: String,
    replace: bool,
) -> Result<(), String> {
    desk.save_credentials(api_id, api_hash, replace).await
}

#[tauri::command]
async fn submit_password(
    desk: tauri::State<'_, Arc<Desk>>,
    password: String,
) -> Result<(), String> {
    desk.submit_password(password).await
}

#[tauri::command]
async fn logout(desk: tauri::State<'_, Arc<Desk>>) -> Result<(), String> {
    desk.logout().await
}

#[tauri::command]
fn set_sending(desk: tauri::State<'_, Arc<Desk>>, enabled: bool) -> Result<(), String> {
    desk.set_sending(enabled)
}

#[tauri::command]
fn set_client(
    desk: tauri::State<'_, Arc<Desk>>,
    kind: String,
    enabled: bool,
) -> Result<String, String> {
    let kind = ClientKind::parse(&kind).ok_or_else(|| "неизвестный клиент".to_owned())?;
    desk.set_client(kind, enabled)
}
