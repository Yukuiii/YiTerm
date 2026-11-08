mod ipc;
mod sessions;
mod settings;
mod state;
mod terminal;
mod translate;

use std::sync::OnceLock;

use tauri::Manager;
use tracing_subscriber::layer::SubscriberExt;

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            init_logging(app.handle().clone());
            let state = state::AppState::init(app.handle().clone());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            ipc::terminal_start,
            ipc::terminal_write,
            ipc::terminal_send_prompt,
            ipc::terminal_attach,
            ipc::terminal_resize,
            ipc::terminal_kill,
            ipc::conversations_list,
            ipc::conversations_create,
            ipc::conversations_delete,
            ipc::chat_save,
            ipc::chat_load,
            ipc::chat_image_save,
            ipc::chat_attachment_import,
            ipc::debug_log,
            ipc::current_user,
            ipc::settings_get,
            ipc::settings_set,
            ipc::api_key_get,
            ipc::api_key_set,
            ipc::translate,
            ipc::model_status,
            ipc::model_download,
            ipc::model_delete,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");

    let _ = app;
}

static _LOG_GUARD: OnceLock<()> = OnceLock::new();

fn init_logging(app: tauri::AppHandle) {
    let log_dir = dirs::home_dir()
        .map(|h| h.join("Library/Logs/com.yiterm.app"))
        .expect("home dir");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(log_dir, "yiterm.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    // Keep the guard alive for the process lifetime.
    _LOG_GUARD.get_or_init(|| {
        std::mem::forget(guard);
    });
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let stdout_layer = tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_level(true)
        .compact();
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false);
    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(stdout_layer)
        .with(file_layer);
    tracing::subscriber::set_global_default(subscriber).ok();
    tracing::info!(?app, "YiTerm starting");
}
