#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod libwindow;
mod models;
mod pipe_source;
mod popup;
mod preview;
mod shell;
mod service;
mod source;
mod status;
mod tier1;
mod watcher;

use commands::AppState;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

/// Prefer the real index (searchd); fall back to generated demo data when it is not running.
fn pick_source() -> Arc<dyn source::NameSource> {
    match pipe_source::PipeSource::connect(Duration::from_millis(300)) {
        Ok(p) => Arc::new(p),
        Err(e) => {
            eprintln!("winseek: searchd not reachable ({e}); using demo data");
            Arc::new(source::MockSource::generate(200_000))
        }
    }
}

fn main() {
    let hotkey = Shortcut::new(Some(Modifiers::ALT), Code::Space);

    tauri::Builder::default()
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, sc, ev| {
                    if *sc == hotkey && ev.state() == ShortcutState::Pressed {
                        popup::toggle(app);
                    }
                })
                .build(),
        )
        .manage(models::Models::default())
        .manage(tier1::library::LibState::default())
        .manage(AppState {
            source: RwLock::new(pick_source()),
            paused: AtomicBool::new(false),
        })
        .invoke_handler(tauri::generate_handler![
            commands::search,
            commands::status,
            commands::recent,
            commands::open_path,
            commands::reveal_path,
            commands::copy_path,
            preview::preview_info,
            shell::copy_files,
            shell::recycle,
            shell::restore_recycled,
            shell::rename_path,
            shell::show_context_menu,
            shell::start_drag,
            service::service_state,
            service::install_service,
            service::start_service,
            service::accent_palette,
            tier1::tier1_status,
            tier1::tier1_set_scopes,
            tier1::tier1_pause,
            tier1::tier1_get_ignore,
            tier1::tier1_set_ignore,
            tier1::semantic_search,
            tier1::library::library_images,
            tier1::library::library_collections,
            tier1::library::library_set_collections,
            tier1::library::library_collection,
            tier1::library::library_dup_groups,
            tier1::library::library_near_start,
            tier1::library::library_near_poll,
            tier1::library::library_big_files,
            tier1::library::library_dims,
            libwindow::open_library,
            status::status_page,
            status::rescan_volume,
            status::tier1_rebuild,
            models::models_plan,
            models::models_download,
            models::models_cancel
        ])
        .register_asynchronous_uri_scheme_protocol("wsicon", |_ctx, req, responder| {
            shell::icons::handle(req, responder)
        })
        .register_asynchronous_uri_scheme_protocol("wsfile", |_ctx, req, responder| {
            preview::serve(req, responder)
        })
        .on_window_event(|win, ev| {
            if win.label() == popup::LABEL {
                match ev {
                    WindowEvent::Focused(false) => popup::on_blur(win.app_handle()),
                    // Never destroy the pre-warmed window.
                    WindowEvent::CloseRequested { api, .. } => {
                        api.prevent_close();
                        popup::hide(win.app_handle());
                    }
                    _ => {}
                }
            }
        })
        .setup(move |app| {
            app.manage(tier1::init());
            if let Ok(t) = app.state::<tier1::Handle>().get() {
                tier1::policy::spawn(t); // pause on battery / full-screen, resume by itself
            }
            if let Some(w) = popup::window(app.handle()) {
                popup::apply_backdrop(&w);
            }
            if let Err(e) = app.global_shortcut().register(hotkey) {
                eprintln!("winseek: could not register Alt+Space: {e}");
            }

            let show = MenuItemBuilder::with_id("show", "Show winseek").build(app)?;
            let pause = MenuItemBuilder::with_id("pause", "Pause indexing").build(app)?;
            app.manage(watcher::PauseItem(pause.clone()));
            let library = MenuItemBuilder::with_id("library", "Library").build(app)?;
            let status = MenuItemBuilder::with_id("status", "Status").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &library, &pause, &status, &quit]).build()?;
            let mut tray = TrayIconBuilder::with_id(watcher::TRAY_ID)
                .tooltip("winseek (Alt+Space)")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |app, ev| match ev.id().as_ref() {
                    "show" => popup::show(app),
                    "library" => {
                        if let Err(e) = libwindow::open(app) {
                            eprintln!("winseek: could not open the library: {e}");
                        }
                    }
                    "status" => {
                        popup::show(app);
                        let _ = app.emit("open-status", ());
                    }
                    "pause" => {
                        // Flips the flag status() reports and pauses the Tier 1 worker.
                        let st = app.state::<AppState>();
                        // Toggle the user's pause (the panel may have changed it behind our back).
                        let now = match app.state::<tier1::Handle>().get() {
                            Ok(t) => !matches!(t.paused_reason(), Some(tier1::policy::PauseReason::User)),
                            Err(_) => !st.paused.load(Ordering::Relaxed),
                        };
                        st.paused.store(now, Ordering::Relaxed);
                        if let Ok(t) = app.state::<tier1::Handle>().get() {
                            t.set_paused(now);
                        }
                        watcher::refresh_pause_item(app);
                    }
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, ev| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = ev
                    {
                        popup::show(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            watcher::spawn(app.handle().clone());
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running winseek app");
}
