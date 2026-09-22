//! Application setup: modules, plugins, commands, startup work and shutdown.

pub mod cleanup;
pub mod commands;
pub mod config;
pub mod core;
pub mod desktop_integration;
pub mod devices;
pub mod devlog;
pub mod fs_ops;
pub mod listing;
pub mod local_path;
pub mod open;
pub mod os_clipboard;
// Native drag-out needs the GTK window, which only exists on Linux.
#[cfg(target_os = "linux")]
pub mod os_drag;
pub mod runpod;
pub mod thumbs;
pub mod transfers;
pub use core::*;

/// Builds the Tauri application and runs it until the window closes.
pub fn run() {
    // WebKitGTK cannot allocate DMABUF buffers on some GPU drivers and the
    // window stays blank; the fallback renderer draws correctly.
    if std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none() {
        // SAFETY: still single-threaded; the webview and async runtime have not started.
        unsafe { std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1") };
    }

    // GTK's overlay scrollbars are drawn above the page and cannot be styled
    // or layered from CSS; classic scrollbars are.
    if std::env::var_os("GTK_OVERLAY_SCROLLING").is_none() {
        // SAFETY: same as above.
        unsafe { std::env::set_var("GTK_OVERLAY_SCROLLING", "0") };
    }

    tauri::Builder::default()
        // Restores window position, size and maximized state across launches.
        .plugin(tauri_plugin_window_state::Builder::default().build())
        // JSON store shared by the frontend's persisted state and the Rust config.
        .plugin(tauri_plugin_store::Builder::default().build())
        // Writes plain text to the clipboard, e.g. "Copy S3 path".
        .plugin(tauri_plugin_clipboard_manager::init())
        // Every command the frontend may call through `invoke`.
        .invoke_handler(tauri::generate_handler![
            cleanup::scan_volume,
            cleanup::cancel_scan,
            cleanup::delete_scanned,
            commands::test_connection,
            config::load_connection,
            config::save_connection,
            config::list_connections,
            config::active_connection_id,
            config::set_active_connection,
            config::rename_connection,
            config::delete_connection,
            config::set_runpod_api_key,
            config::has_runpod_api_key,
            runpod::volume_quota,
            desktop_integration::is_open_here_registered,
            desktop_integration::register_open_here,
            desktop_integration::take_startup_path,
            devices::list_devices,
            devices::disk_usage,
            devices::local_home_dir,
            devlog::devlog_toast,
            devlog::devlog_verbose,
            fs_ops::create_folder,
            fs_ops::create_file,
            fs_ops::rename,
            fs_ops::delete,
            fs_ops::folder_size,
            listing::list_remote,
            listing::list_local,
            listing::stat_paths,
            open::open_path,
            open::open_url,
            os_clipboard::read_os_clipboard_files,
            os_clipboard::write_os_clipboard_files,
            #[cfg(target_os = "linux")]
            os_drag::start_os_drag,
            thumbs::get_thumbnail,
            transfers::transfer_start,
            transfers::transfer_zip_start,
            transfers::open_remote_start,
            transfers::transfer_cancel,
            transfers::transfer_cancel_all,
            transfers::list_orphan_uploads,
            transfers::abort_orphan_uploads
        ])
        // Shared state: the transfer queue, the cleanup scan's cancel flag, and
        // a path handed over by macOS "Open With".
        .manage(transfers::TransferManager::new())
        .manage(cleanup::CleanupState::new())
        .manage(desktop_integration::OpenHereState::default())
        .setup(|app| {
            devlog::init(app.handle());
            // Aborts unfinished multipart uploads older than 7 days, in the background.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(transfers::sweep_old_orphans(handle));
            // Empties the cache of remote files downloaded only to be opened.
            transfers::clear_opened_cache(app.handle());
            Ok(())
        })
        .on_window_event(|window, event| match event {
            // Saves pending settings before the process exits below.
            tauri::WindowEvent::CloseRequested { .. } => {
                use tauri::Manager;
                use tauri_plugin_store::StoreExt;
                if let Ok(store) = window.app_handle().store("app-state.json") {
                    let _ = store.save();
                }
            }
            // Exits immediately once the window is gone, after saving window state and flushing the log.
            tauri::WindowEvent::Destroyed => {
                use tauri::Manager;
                use tauri_plugin_window_state::{AppHandleExt as _, StateFlags};
                let _ = window.app_handle().save_window_state(StateFlags::all());
                devlog::flush();
                std::process::exit(0);
            }
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("failed to start the Tauri application")
        .run(|app, event| {
            // `app` is only used on macOS.
            let _ = &app;
            // Same fast exit if the app ends without the window being destroyed first.
            if let tauri::RunEvent::Exit = event {
                devlog::flush();
                std::process::exit(0);
            }
            // macOS delivers "Open With" as an event rather than a command-line argument.
            #[cfg(target_os = "macos")]
            if let tauri::RunEvent::Opened { urls } = &event {
                use tauri::{Emitter, Manager};
                if let Some(path) = urls.first().and_then(|u| u.to_file_path().ok()) {
                    let path = path.to_string_lossy().into_owned();
                    if let Some(state) = app.try_state::<desktop_integration::OpenHereState>() {
                        *state.0.lock().unwrap() = Some(path.clone());
                    }
                    let _ = app.emit("open-here", path);
                }
            }
        });
}
