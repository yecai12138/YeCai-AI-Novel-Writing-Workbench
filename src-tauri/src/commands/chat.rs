//! 会话历史的 4 个命令：保存一条 / 读整段 / 清空 / 取最近一次的 session_id。
//!
//! 路径只收 `book_root`（书根，`.active-book` 指到的那本），会话文件固定落在
//! `<书根>/.storydesk/chats/`。`session_id` 的合法性由 [`chat_store::session_path`] 把关。

use std::path::PathBuf;

use crate::models::chat::ChatMessage;
use crate::models::StoryError;
use crate::services::chat_store;

use super::blocking;

/// 书根必须是个真实目录；返回规范化后的绝对路径。
fn resolve_book(book_root: &str) -> Result<PathBuf, StoryError> {
    let raw = book_root.trim();
    if raw.is_empty() {
        return Err(StoryError::invalid("bookRoot", "书根不能为空"));
    }
    let path = PathBuf::from(raw);
    if !path.is_dir() {
        return Err(StoryError::invalid(
            "bookRoot",
            format!("书根不存在或不是目录: {}", path.display()),
        ));
    }
    std::fs::canonicalize(&path)
        .map_err(|e| StoryError::fs(path.display().to_string(), format!("解析书根失败: {e}")))
}

/// 追加（或按 id 覆盖）一条会话消息。
#[tauri::command]
pub async fn save_chat_message(
    book_root: String,
    session_id: String,
    message: ChatMessage,
) -> Result<(), StoryError> {
    blocking(move || {
        let book = resolve_book(&book_root)?;
        chat_store::append(&book, &session_id, &message)
    })
    .await
}

/// 读整段会话（文件不存在返回空数组）。
#[tauri::command]
pub async fn load_chat_history(
    book_root: String,
    session_id: String,
) -> Result<Vec<ChatMessage>, StoryError> {
    blocking(move || {
        let book = resolve_book(&book_root)?;
        // 先校验 id，读路径与写路径共用同一套规则，不给「只读就绕过」留口子。
        chat_store::session_path(&book, &session_id)?;
        Ok(chat_store::load(&book, &session_id))
    })
    .await
}

/// 清空当前会话（写回 `[]`，历史文件仍留着）。
#[tauri::command]
pub async fn clear_chat_history(book_root: String, session_id: String) -> Result<(), StoryError> {
    blocking(move || {
        let book = resolve_book(&book_root)?;
        chat_store::clear(&book, &session_id)
    })
    .await
}

/// 最近一次会话的 id（应用启动时用它恢复上次对话）。
#[tauri::command]
pub async fn latest_chat_session(book_root: String) -> Result<Option<String>, StoryError> {
    blocking(move || {
        let book = resolve_book(&book_root)?;
        Ok(chat_store::latest_session(&book))
    })
    .await
}
