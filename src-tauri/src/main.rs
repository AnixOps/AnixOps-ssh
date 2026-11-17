// Hides the extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

mod askpass;
mod backup;
mod daemon;
mod files;
mod hosts;
mod import;
mod known_hosts;
mod sessions;
mod settings;
mod snippets;
mod telemetry;
mod tunnels;
mod vault;

fn main() {
    // OpenSSH starts this executable to ask for a password; see askpass.rs.
    askpass::run_helper_if_requested();
    // Crash capture first, so a panic in the daemon below is caught too.
    telemetry::init();
    // Or, with `--easysshd`, the background session daemon; see daemon.rs.
    daemon::run_if_requested();

    tauri::Builder::default()
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(daemon::DaemonState::default())
        .manage(sessions::Sessions::default())
        .manage(files::Files::default())
        .manage(askpass::Askpass::default())
        .manage(tunnels::Tunnels::default())
        .invoke_handler(tauri::generate_handler![
            hosts::load_hosts,
            hosts::open_external,
            hosts::host_save,
            hosts::host_delete,
            hosts::host_set_favorite,
            import::import_preview_file,
            import::import_preview_ssh_config,
            import::import_apply_hosts,
            known_hosts::known_hosts_list,
            known_hosts::known_hosts_import,
            known_hosts::known_hosts_remove,
            snippets::snippets_list,
            snippets::snippet_save,
            snippets::snippet_delete,
            tunnels::tunnels_list,
            tunnels::tunnel_save,
            tunnels::tunnel_delete,
            tunnels::tunnel_start,
            tunnels::tunnel_stop,
            settings::settings_get,
            settings::settings_save,
            settings::ssh_status,
            telemetry::telemetry_status,
            telemetry::telemetry_pending,
            telemetry::telemetry_delete_pending,
            telemetry::telemetry_report_frontend,
            backup::config_export,
            backup::config_import,
            backup::sync_get,
            backup::sync_setup,
            backup::sync_push,
            backup::sync_pull,
            sessions::session_open,
            sessions::session_write,
            sessions::session_resize,
            sessions::session_close,
            sessions::sessions_list,
            sessions::session_attach,
            sessions::session_detach,
            files::sftp_open,
            files::sftp_list,
            files::sftp_mkdir,
            files::sftp_remove,
            files::sftp_rename,
            files::sftp_close,
            files::transfer_start,
            files::transfer_cancel,
            askpass::askpass_reply,
            askpass::askpass_unlock,
            vault::vault_status,
            vault::vault_unlock,
            vault::vault_lock,
            vault::vault_set_passphrase,
            vault::vault_remove_passphrase,
            vault::vault_set_auto_lock,
            vault::vault_forget_all,
            vault::secrets_status,
            vault::secrets_set
        ])
        .setup(|app| {
            daemon::start(app.handle());
            telemetry::start_background();
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("failed to build AnixOps SSH")
        .run(|app, event| {
            // Background ssh processes do not end with the window on their own.
            if let tauri::RunEvent::Exit = event {
                app.state::<tunnels::Tunnels>().stop_all();
            }
        });
}
