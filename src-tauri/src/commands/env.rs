use tauri::command;

use crate::models::dto::EnvReport;
use crate::models::StoryError;
use crate::services::env_check;

/// 环境自检：node(>=18) / python3 / bash(Git Bash) / 宿主 Agent CLI。
#[command]
pub async fn env_check() -> Result<EnvReport, StoryError> {
    super::blocking(|| Ok(env_check::check_all())).await
}
