mod commands;
mod models;
mod services;

// `tauri::is_dev()` 就是 `!cfg!(feature = "custom-protocol")`（tauri/src/lib.rs:317），
// 与 generate_context! 判定是否内嵌前端的条件同源。release 里若仍为 dev，
// 前端资源不会被内嵌，窗口直接加载 devUrl(http://localhost:1420)，用户双击得到
// ERR_CONNECTION_REFUSED（2026-11-17 事故）。`tauri build` 会带 `--features tauri/custom-protocol`。
#[cfg(not(debug_assertions))]
const _: () = assert!(
    !tauri::is_dev(),
    "release 构建必须带 `--features tauri/custom-protocol`（用 `npm run tauri:build`），否则窗口会去连 http://localhost:1420"
);

use tauri::Manager;

use services::engine::EngineState;
use services::watcher::WatcherState;

pub fn run() {
    tauri::Builder::default()
        .manage(WatcherState::new())
        .manage(EngineState::new())
        .setup(|app| {
            // Windows / Linux 上 Tauri 默认不创建菜单栏（这里也没有创建任何菜单），
            // 因此不存在“默认菜单”需要关闭；只有 macOS 会注入默认 App 菜单，故仅在那里移除。
            #[cfg(target_os = "macos")]
            {
                app.remove_menu()?;
            }
            // 窗口标题栏也带版本号：与「关于」页同一个来源（tauri.conf.json 的 version，
            // 由 package_info 读回来），不另存一份手写字符串。窗口不在时静默跳过。
            let info = app.package_info();
            let title = format!("{} v{}", info.name, info.version);
            if let Some(win) = app.get_webview_window("main") {
                if let Err(e) = win.set_title(&title) {
                    eprintln!("设置窗口标题失败：{e}");
                }
            }
            let _ = app;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::env::env_check,
            commands::event::event_list,
            commands::event::event_get,
            commands::event::event_audit,
            commands::project::new_project,
            commands::project::open_project,
            commands::project::list_tree,
            commands::project::list_books,
            commands::project::read_active_book,
            commands::project::set_active_book,
            commands::project::list_dirs,
            commands::project::validate_path_cmd,
            commands::fs::read_file,
            commands::fs::write_file,
            commands::fs::create_chapter,
            commands::fs::create_note,
            commands::fs::rename_entry,
            commands::fs::move_entry,
            commands::fs::delete_entry,
            commands::fs::list_snapshots,
            commands::fs::delete_snapshot,
            commands::fs::restore_snapshot,
            commands::fs::watch_project,
            commands::fs::unwatch_project,
            commands::ai::ai_start,
            commands::ai::ai_cancel,
            commands::ai::ai_running,
            commands::ai::ai_engines,
            commands::ai::engine_install_opencode,
            commands::ai::opencode_storage,
            commands::ai::opencode_engine_info,
            commands::ops::ops_overview,
            commands::ops::ops_clean_logs,
            commands::ops::ops_vacuum_database,
            commands::ops::ops_clean_repos,
            commands::ai::runtime_install,
            commands::ai::agent_specs,
            commands::ai::agent_spec,
            commands::ai::gate_probe,
            commands::ai::gate_chapter_check,
            commands::ai::gate_postcheck,
            commands::ai::gate_build_packet,
            commands::style::style_stats,
            commands::master::master_plan,
            commands::master::master_run,
            commands::master::master_resume,
            commands::master::master_execute,
            commands::master::master_agents,
            commands::master::master_default_agents,
            commands::prompt::polish_prompt,
            commands::chat::save_chat_message,
            commands::chat::load_chat_history,
            commands::chat::clear_chat_history,
            commands::chat::latest_chat_session,
            commands::settings::get_settings,
            commands::settings::save_settings,
            commands::settings::set_provider_key,
            commands::settings::check_provider_key,
            commands::settings::delete_provider_key,
            commands::settings::delete_provider,
            commands::settings::fetch_available_models,
            commands::settings::validate_provider,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
