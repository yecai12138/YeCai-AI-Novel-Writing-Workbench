//! 事件卡的只读查询命令。
//!
//! S1 只做三件事：把 `书根/大纲/事件/*.md` 读进来、解析成结构、交给前端展示。
//! 不写盘、不改生成流程、不改门禁——先把「事件」变成界面上看得见、
//! 后续接缝（上下文注入 / 排程 / 门禁）拿得到的东西。
//!
//! 单张卡解析失败不影响整批：错误随列表返回，用户自己改那张卡即可。

use std::path::{Path, PathBuf};

use tauri::command;

use crate::models::event::{self, EventCard, EventList, EventParseError, EventSummary};
use crate::models::StoryError;
use crate::services::path_guard;

/// 事件卡目录（相对书根）。与取料层共用同一个常量，避免两处各写一份。
const EVENT_DIR_REL: &str = crate::services::context::EVENT_DIR_REL;

/// 校验事件目录并返回绝对路径；目录可以不存在（老书没有事件层），这里不报错。
fn event_dir(book_root: &Path) -> Result<PathBuf, StoryError> {
    path_guard::validate_path(book_root, &book_root.join(EVENT_DIR_REL))
}

/// 书根内的绝对路径 → 相对书根的 `/` 分隔路径。
fn relative_slash(book_root: &Path, path: &Path) -> String {
    let rel = path.strip_prefix(book_root).unwrap_or(path);
    rel.to_string_lossy().replace('\\', "/")
}

fn is_card_file(path: &Path) -> bool {
    path.is_file()
        && path
            .extension()
            .map(|ext| ext.eq_ignore_ascii_case("md"))
            .unwrap_or(false)
}

/// 目录里的 `*.md`，按文件名排序（章号在文件名里，排序即章序）。
fn card_files(dir: &Path) -> Result<Vec<PathBuf>, StoryError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| StoryError::fs(dir.display().to_string(), format!("读事件目录失败: {e}")))?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_card_file(path))
        .collect();
    files.sort();
    Ok(files)
}

/// 读整批事件卡。单张卡解析失败不影响其余卡：错误进 `errors`。
pub(crate) fn load_list(root: &Path) -> Result<EventList, StoryError> {
    let dir = event_dir(root)?;
    let mut cards: Vec<EventSummary> = Vec::new();
    let mut errors: Vec<EventParseError> = Vec::new();
    if !dir.is_dir() {
        return Ok(EventList { cards, errors });
    }
    for path in card_files(&dir)? {
        let rel = relative_slash(root, &path);
        match std::fs::read_to_string(&path) {
            Ok(body) => match event::parse_card(&rel, &body) {
                Ok(card) => cards.push(EventSummary::from(&card)),
                Err(message) => errors.push(EventParseError { path: rel, message }),
            },
            Err(e) => errors.push(EventParseError {
                path: rel,
                message: format!("读文件失败: {e}"),
            }),
        }
    }
    cards.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(EventList { cards, errors })
}

/// 按事件号取一张卡；`event_id` 允许 `2` / `002` / `事件_002` / 带路径四种写法。
pub(crate) fn load_card(root: &Path, event_id: &str) -> Result<EventCard, StoryError> {
    let dir = event_dir(root)?;
    let wanted = event::normalise_id(event_id)
        .ok_or_else(|| StoryError::invalid("eventId", format!("`{event_id}` 不是事件号")))?;
    if !dir.is_dir() {
        return Err(StoryError::invalid(
            "eventId",
            format!("事件目录不存在（{EVENT_DIR_REL}）"),
        ));
    }
    for path in card_files(&dir)? {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if event::id_from_file_name(name) != Some(wanted.clone()) {
            continue;
        }
        let rel = relative_slash(root, &path);
        let body = std::fs::read_to_string(&path)
            .map_err(|e| StoryError::fs(rel.clone(), format!("读文件失败: {e}")))?;
        return event::parse_card(&rel, &body)
            .map_err(|message| StoryError::invalid("eventId", format!("{rel}: {message}")));
    }
    Err(StoryError::invalid(
        "eventId",
        format!("找不到事件 {wanted}（{EVENT_DIR_REL}/ 下没有对应文件）"),
    ))
}

/// 列出书里的全部事件卡。
#[command]
pub async fn event_list(book_root: String) -> Result<EventList, StoryError> {
    super::blocking(move || load_list(Path::new(&book_root))).await
}

/// 取一张事件卡的完整内容（含未结项、账本、章型分工）。
#[command]
pub async fn event_get(book_root: String, event_id: String) -> Result<EventCard, StoryError> {
    super::blocking(move || load_card(Path::new(&book_root), &event_id)).await
}

/// 事件层反向审计：卡是不是空的（一章一事 / 账本从没动过 / 细纲和卡对不上）。
///
/// 只读。没有事件卡的书返回 `cards: 0` 与空 findings，老书不受打扰。
#[command]
pub async fn event_audit(book_root: String) -> Result<crate::services::event_audit::EventAudit, StoryError> {
    super::blocking(move || crate::services::event_audit::audit(Path::new(&book_root))).await
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: &str = "# 事件 002 · 码头交接

## 基本信息

| 项 | 值 |
| --- | --- |
| 所属卷 | 1 |
| 章号区间 | 1-5 |
| 前置事件 | — |
| 状态 | 进行 |

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
| --- | --- | --- | --- | --- |
| U-001 | 仓单第三联不在 | 1 | 4 | 第三联 |

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
| --- | --- | --- | --- |
| 1 | 起 | — | U-001 |
";

    fn scratch(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("storydesk-event-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(EVENT_DIR_REL)).unwrap();
        root
    }

    #[test]
    fn lists_cards_and_collects_parse_errors() {
        let root = scratch("list");
        std::fs::write(root.join(EVENT_DIR_REL).join("事件_002_码头交接.md"), CARD).unwrap();
        std::fs::write(root.join(EVENT_DIR_REL).join("事件_x_没号.md"), "# 事件 · 没号
").unwrap();
        std::fs::write(root.join(EVENT_DIR_REL).join("说明.txt"), "忽略我").unwrap();
        let list = load_list(&root).unwrap();
        assert_eq!(list.cards.len(), 1);
        assert_eq!(list.cards[0].id, "002");
        assert_eq!(list.cards[0].open_count, 1);
        assert_eq!(list.cards[0].role_count, 1);
        assert_eq!(list.errors.len(), 1);
        assert!(list.errors[0].path.ends_with("事件_x_没号.md"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_event_dir_is_an_empty_list_not_an_error() {
        let root = std::env::temp_dir().join("storydesk-event-none");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let list = load_list(&root).unwrap();
        assert!(list.cards.is_empty() && list.errors.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn get_accepts_loose_ids_and_reports_missing() {
        let root = scratch("get");
        std::fs::write(root.join(EVENT_DIR_REL).join("事件_002_码头交接.md"), CARD).unwrap();
        for wanted in ["002", "2", "事件_002", "大纲/事件/事件_002_码头交接.md"] {
            let card = load_card(&root, wanted).unwrap();
            assert_eq!(card.id, "002", "{wanted}");
            assert_eq!(card.roles.len(), 1);
        }
        let err = load_card(&root, "099").unwrap_err();
        let text = format!("{err:?}");
        assert!(text.contains("找不到事件 099"), "{text}");
        let _ = std::fs::remove_dir_all(&root);
    }
}
