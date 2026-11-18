pub mod ai;
pub mod chat;
pub mod env;
pub mod event;
pub mod fs;
pub mod master;
pub mod ops;
pub mod prompt;
pub mod project;
pub mod settings;

use crate::models::StoryError;

/// 把阻塞型文件/进程操作挪到阻塞线程池，避免 sync command 卡住主线程。
pub async fn blocking<T, F>(f: F) -> Result<T, StoryError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, StoryError> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| StoryError::internal(format!("后台任务失败: {e}")))?
}
