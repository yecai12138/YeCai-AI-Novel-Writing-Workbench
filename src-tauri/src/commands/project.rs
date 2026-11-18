use std::path::{Path, PathBuf};

use tauri::command;

use crate::models::dto::{DirListing, ProjectInfo, TreeNode};
use crate::models::StoryError;
use crate::services::{dir_guard, workspace};

/// 打开/切换书之后装一遍 hook（幂等）。
///
/// 第 037 章的正文是写手子智能体用 PowerShell 写坏的，而老 hook 的 matcher 只有
/// `Write|Edit|MultiEdit`。装 hook 原来只发生在编排流程里，用户不点编排就一直没有这层防护；
/// 打开书的时候补一次，代价是两张小文件写盘，失败也不挡打开。
fn ensure_hooks(info: &ProjectInfo) {
    if let Err(e) = dir_guard::ensure(Path::new(&info.book_root)) {
        eprintln!("[storydesk] hook 没装上（不挡打开）：{e}");
    }
}

/// 新建项目：在工作区根下创建 `长篇/<书名>/` 标准结构，并写 `.active-book`。
#[command]
pub async fn new_project(
    workspace_dir: String,
    book_name: String,
) -> Result<ProjectInfo, StoryError> {
    super::blocking(move || {
        let dir = PathBuf::from(workspace_dir);
        workspace::create_project(&dir, &book_name)
    })
    .await
}

/// 打开项目：读 `.active-book` 定位书根（缺失时按 长篇/ 下唯一书目回填）。
#[command]
pub async fn open_project(workspace_dir: String) -> Result<ProjectInfo, StoryError> {
    let info = super::blocking(move || {
        let dir = PathBuf::from(workspace_dir);
        workspace::open_project(&dir)
    })
    .await?;
    ensure_hooks(&info);
    Ok(info)
}

/// 目录树：返回书根下的目录与文件节点。
#[command]
pub async fn list_tree(book_root: String) -> Result<TreeNode, StoryError> {
    super::blocking(move || {
        let root = PathBuf::from(book_root);
        workspace::build_tree(&root)
    })
    .await
}

/// 列出工作区内所有可识别的书（`长篇/<x>` / `短篇/<x>`）。
#[command]
pub async fn list_books(workspace_dir: String) -> Result<Vec<String>, StoryError> {
    super::blocking(move || {
        let dir = PathBuf::from(workspace_dir);
        workspace::list_books(&dir)
    })
    .await
}

/// 读取当前 `.active-book` 内容。
#[command]
pub async fn read_active_book(workspace_dir: String) -> Result<Option<String>, StoryError> {
    super::blocking(move || {
        let dir = PathBuf::from(workspace_dir);
        workspace::read_active_book(&dir)
    })
    .await
}

/// 切换当前书（重写 `.active-book`）。
#[command]
pub async fn set_active_book(
    workspace_dir: String,
    active_book: String,
) -> Result<ProjectInfo, StoryError> {
    let info = super::blocking(move || {
        let dir = PathBuf::from(workspace_dir);
        workspace::set_active_book(&dir, &active_book)
    })
    .await?;
    ensure_hooks(&info);
    Ok(info)
}

/// 目录选择器：列举子目录（空字符串代表列举盘符）。
#[command]
pub async fn list_dirs(path: String) -> Result<DirListing, StoryError> {
    super::blocking(move || workspace::list_dirs(&path)).await
}

/// 路径守卫自测入口：前端可用来验证越界路径会被拒绝。
#[command]
pub async fn validate_path_cmd(
    workspace_root: String,
    target: String,
) -> Result<String, StoryError> {
    super::blocking(move || {
        let root = PathBuf::from(workspace_root);
        let resolved = crate::services::path_guard::validate_path(&root, &PathBuf::from(target))?;
        Ok(resolved.display().to_string())
    })
    .await
}
