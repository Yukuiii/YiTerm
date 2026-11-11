mod git;
mod ipc;
mod sessions;
mod settings;
mod state;
mod terminal;
mod translate;
mod update;

use std::sync::OnceLock;

use tauri::Manager;
use tracing_subscriber::layer::SubscriberExt;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            init_logging(app.handle().clone());
            let state = state::AppState::init(app.handle().clone());
            app.manage(state);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            git::git_status,
            git::git_diff,
            git::git_history,
            git::git_commit_details,
            ipc::terminal_start,
            ipc::terminal_write,
            ipc::terminal_send_prompt,
            ipc::plan_approval_status,
            ipc::plan_approval_decide,
            ipc::terminal_attach,
            ipc::terminal_resize,
            ipc::terminal_kill,
            ipc::conversations_list,
            ipc::conversations_create,
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
            ipc::translate_cancel,
            ipc::model_status,
            ipc::model_download,
            ipc::model_delete,
            update::update_check,
            update::update_download,
            update::update_install,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

/// Holds the non-blocking log writer's guard for the process lifetime: dropping
/// it would stop the writer thread and silently truncate the log file.
static LOG_GUARD: OnceLock<tracing_appender::non_blocking::WorkerGuard> = OnceLock::new();

fn init_logging(app: tauri::AppHandle) {
    let log_dir = dirs::home_dir()
        .map(|h| h.join("Library/Logs/com.yiterm.app"))
        .expect("home dir");
    let _ = std::fs::create_dir_all(&log_dir);
    let file_appender = tracing_appender::rolling::daily(log_dir, "yiterm.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let _ = LOG_GUARD.set(guard);
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
