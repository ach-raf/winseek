#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod autostart;
mod commands;
mod hotkeys;
mod launch;
mod libwindow;
mod models;
mod pipe_source;
mod popup;
mod preview;
mod shell;
mod service;
mod source;
mod status;
mod textdec;
mod tier1;
mod watcher;

use commands::AppState;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;
use tauri::menu::{MenuBuilder, MenuItemBuilder};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{Emitter, Manager, WindowEvent};
use tauri_plugin_global_shortcut::ShortcutState;

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
    // Started from the Run key at login: stay in the tray. Any other launch (Start menu, installer,
    // a second double-click) shows the popup.
    let autostarted = std::env::args().any(|a| a == autostart::ARG);

    tauri::Builder::default()
        // must be the first plugin: a second launch hands over to the running instance and exits
        .plugin(tauri_plugin_single_instance::init(|app, argv, _cwd| {
            if !argv.iter().any(|a| a == autostart::ARG) {
                popup::show(app);
            }
        }))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                // The handler reads the CURRENT bindings, never a captured constant: Settings can
                // rebind while the app runs (PRD RB4).
                .with_handler(|app, sc, ev| {
                    if ev.state() != ShortcutState::Pressed {
                        return;
                    }
                    let Some(b) = app.try_state::<hotkeys::Hotkeys>() else { return };
                    let combo = hotkeys::combo(sc);
                    let b = b.current();
                    if b.popup.as_deref() == Some(combo.as_str()) {
                        popup::toggle(app);
                    } else if b.library.as_deref() == Some(combo.as_str()) {
                        if let Err(e) = libwindow::open(app) {
                            eprintln!("winseek: could not open the library: {e}");
                        }
                    } else if b.clipboard.as_deref() == Some(combo.as_str()) {
                        hotkeys::show_with_clipboard(app);
                    }
                })
                .build(),
        )
        .manage(models::Models::default())
        .manage(tier1::library::LibState::default())
        .manage(launch::Launcher::new())
        .manage(Arc::new(agent::AgentState::load()))
        .manage(AppState::new(pick_source()))
        .invoke_handler(tauri::generate_handler![
            agent::agent_get_config,
            agent::agent_set_config,
            agent::agent_log,
            agent::agent_clear_log,
            agent::agent_mcp_path,
            commands::search,
            commands::status,
            commands::recent,
            commands::open_path,
            commands::reveal_path,
            commands::copy_path,
            preview::preview_info,
            preview::preview_pdf_page,
            preview::preview_pdf_boxes,
            shell::copy_files,
            shell::copy_text_html,
            shell::recycle,
            shell::restore_recycled,
            shell::rename_path,
            shell::show_context_menu,
            shell::show_properties,
            shell::start_drag,
            popup::move_popup,
            popup::open_popup,
            service::service_state,
            service::install_service,
            service::start_service,
            service::accent_palette,
            tier1::tier1_status,
            tier1::tier1_set_scopes,
            tier1::tier1_remove_folder,
            tier1::tier1_pause,
            tier1::tier1_get_ignore,
            tier1::tier1_set_ignore,
            tier1::tier1_get_index_code,
            tier1::tier1_set_index_code,
            tier1::semantic_search,
            tier1::parse_document,
            tier1::parser_present,
            tier1::get_profile,
            tier1::set_profile,
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
            models::models_cancel,
            autostart::autostart_get,
            autostart::autostart_set,
            hotkeys::hotkeys_get,
            hotkeys::hotkeys_set,
            launch::launch_search,
            launch::launch_run,
            launch::launch_record,
            launch::launch_reindex,
            launch::launch_get_config,
            launch::launch_set_config,
            launch::launch_hide,
            launch::launch_unhide,
            launch::launch_clear_history,
            launch::launch_counts,
            launch::launch_copy,
            launch::launch_reveal
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
                    WindowEvent::Moved(pos) => popup::on_moved(win, *pos),
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
            app.manage(tier1::init()); // also starts the battery / full-screen pause policy
            agent::server::spawn(app.handle().clone());
            hotkeys::init(app.handle()); // registers the (possibly rebound) global hotkeys
            if let Some(w) = popup::window(app.handle()) {
                popup::apply_backdrop(&w);
            }
            let show = MenuItemBuilder::with_id("show", "Show winseek").build(app)?;
            let pause = MenuItemBuilder::with_id("pause", "Pause indexing").build(app)?;
            app.manage(watcher::PauseItem(pause.clone()));
            let library = MenuItemBuilder::with_id("library", "Library").build(app)?;
            let status = MenuItemBuilder::with_id("status", "Status").build(app)?;
            let quit = MenuItemBuilder::with_id("quit", "Quit").build(app)?;
            let menu = MenuBuilder::new(app).items(&[&show, &library, &pause, &status, &quit]).build()?;
            let popup_combo = app.state::<hotkeys::Hotkeys>().current().popup.unwrap_or_default();
            let mut tray = TrayIconBuilder::with_id(watcher::TRAY_ID)
                .tooltip(format!("winseek ({})", if popup_combo.is_empty() { "no hotkey".into() } else { popup_combo }))
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
            launch::Launcher::start_background(app.handle().clone());
            autostart::init_default();
            models::auto_download(app.handle().clone());
            if !autostarted {
                popup::show(app.handle());
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running winseek app");
}
