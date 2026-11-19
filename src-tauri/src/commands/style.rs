//! 文风体检命令：只读统计，不写任何文件。

use crate::models::StoryError;
use crate::services::stylestat;

/// 全书文风统计。章数不足（< stylestat::MIN_CHAPTERS）时返回 null，
/// 前端据此显示「样本不足」，而不是拿 2 章的数字去误导作者。
#[tauri::command]
pub async fn style_stats(
    workspace_root: String,
    book_root: String,
) -> Result<Option<stylestat::StyleStats>, StoryError> {
    let (_ws, book) = crate::commands::ai::resolve_pair(&workspace_root, &book_root)?;
    crate::commands::blocking(move || Ok(stylestat::stats_of(&book))).await
}
