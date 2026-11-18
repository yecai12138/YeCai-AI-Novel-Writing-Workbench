//! 会话落盘：`<书>/.storydesk/chats/<session_id>.json`，一个文件一段会话。
//!
//! 三条约束：
//! 1. **session_id 只允许 `[a-z0-9_-]`**（≤64 字符）——它是文件名，不做校验就能 `../` 逃逸。
//! 2. **整份重写 + 原子写**（[`atomic_write::write_atomic`]）：聊天文件不大（几十 KB），
//!    整份重写比 append 安全，断电也不会留半截 JSON。
//! 3. **读坏了就当空**：手改坏 JSON 不该让面板打不开，`load` 失败返回空列表并留一行日志。

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use crate::models::chat::ChatMessage;
use crate::models::StoryError;
use crate::services::atomic_write;

/// 会话目录（相对书根）。
pub const CHAT_DIR_REL: &str = ".storydesk/chats";

/// 同一进程内串行化「读 → 改 → 写」。
///
/// 前端会并发落盘（一步派发卡 + 整轮收尾几乎同时到），`append` 是读全量再整份写回，
/// 不加锁就会互相覆盖：先读到的那份把后写入的消息抹掉，历史里就少一条收尾。
static CHAT_LOCK: Mutex<()> = Mutex::new(());

fn lock() -> std::sync::MutexGuard<'static, ()> {
    // 前一持有者 panic 了也要能继续写，凭据比 panic 干净更重要
    CHAT_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// `<book_root>/.storydesk/chats`。
pub fn chats_dir(book_root: &Path) -> PathBuf {
    book_root.join(CHAT_DIR_REL)
}

fn valid_session_id(session_id: &str) -> bool {
    let id = session_id.trim();
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `<book_root>/.storydesk/chats/<session_id>.json`（校验过 id）。
pub fn session_path(book_root: &Path, session_id: &str) -> Result<PathBuf, StoryError> {
    if !valid_session_id(session_id) {
        return Err(StoryError::invalid(
            "sessionId",
            "会话 id 只能用小写字母、数字、- 和 _（最长 64 字符）",
        ));
    }
    Ok(chats_dir(book_root).join(format!("{}.json", session_id.trim())))
}

/// 读整段会话；文件不存在或读坏了都返回空列表。
pub fn load(book_root: &Path, session_id: &str) -> Vec<ChatMessage> {
    let Ok(path) = session_path(book_root, session_id) else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    match serde_json::from_str::<Vec<ChatMessage>>(&raw) {
        Ok(list) => list,
        Err(e) => {
            eprintln!(
                "[storydesk] 会话文件读坏了，按空会话处理：{}（{e}）",
                path.display()
            );
            Vec::new()
        }
    }
}

/// 整份写回（原子写）。不加锁的版本，供已持锁的调用方复用。
fn write_raw(
    book_root: &Path,
    session_id: &str,
    messages: &[ChatMessage],
) -> Result<(), StoryError> {
    let path = session_path(book_root, session_id)?;
    let text = serde_json::to_string_pretty(messages)
        .map_err(|e| StoryError::internal(format!("会话序列化失败: {e}")))?;
    atomic_write::write_atomic(&path, text.as_bytes())
}

/// 整份写回（原子写）。
pub fn save_all(
    book_root: &Path,
    session_id: &str,
    messages: &[ChatMessage],
) -> Result<(), StoryError> {
    let _guard = lock();
    write_raw(book_root, session_id, messages)
}

/// 追加一条消息。
pub fn append(book_root: &Path, session_id: &str, message: &ChatMessage) -> Result<(), StoryError> {
    if !ChatMessage::is_valid_role(&message.role) {
        return Err(StoryError::invalid(
            "role",
            "消息角色只能是 user/master/system/agent",
        ));
    }
    let _guard = lock();
    let mut list = load(book_root, session_id);
    // 同一个 id 重复保存时覆盖，避免前端重试把消息存两遍。
    if let Some(slot) = list.iter_mut().find(|m| m.id == message.id) {
        *slot = message.clone();
    } else {
        list.push(message.clone());
    }
    write_raw(book_root, session_id, &list)
}

/// 清空（保留文件，内容写成 `[]`，方便「历史对话」里还能看到这段会话）。
pub fn clear(book_root: &Path, session_id: &str) -> Result<(), StoryError> {
    save_all(book_root, session_id, &[])
}

/// 最近一次会话 id（按文件 mtime 取最新）。
pub fn latest_session(book_root: &Path) -> Option<String> {
    let dir = chats_dir(book_root);
    let mut best: Option<(u128, String)> = None;
    for entry in fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if !valid_session_id(stem) {
            continue;
        }
        let mtime = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_millis())
            .unwrap_or(0);
        match &best {
            Some((best_mtime, _)) if *best_mtime >= mtime => {}
            _ => best = Some((mtime, stem.to_string())),
        }
    }
    best.map(|(_, id)| id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_id_rejects_escapes() {
        let root = Path::new("C:/book");
        assert!(session_path(root, "abc-123_x").is_ok());
        assert!(session_path(root, "../evil").is_err());
        assert!(session_path(root, "a/b").is_err());
        assert!(session_path(root, "ABC").is_err());
        assert!(session_path(root, "").is_err());
        assert!(session_path(root, &"x".repeat(65)).is_err());
    }

    #[test]
    fn session_path_lives_under_storydesk_chats() {
        let root = Path::new("C:/book");
        let p = session_path(root, "s1").unwrap();
        assert!(
            p.ends_with(".storydesk/chats/s1.json") || p.ends_with(".storydesk\\chats\\s1.json")
        );
    }
}
