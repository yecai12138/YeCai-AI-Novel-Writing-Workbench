//! 运维命令：报告日志/会话库/repos 的占用，并执行三种清理。
//!
//! 界面上的三张卡片都从这里取数，删除动作也只在这里发生——
//! 所有动作都会写一行 `engine.log`，事后能查「谁在什么时候删了多少」。
//!
//! 只碰两处：本程序日志目录、第 5 项隔离出来的 opencode 数据目录。
//! 桌面版 `~/.local/share/opencode` 一律不碰。

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;
use tauri::{AppHandle, Manager};

use crate::models::StoryError;
use crate::services::engine;
use crate::services::opencode_home;
use crate::services::ops_cleanup;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsDirView {
    pub path: String,
    pub files: usize,
    pub bytes: u64,
    pub human: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsDbView {
    pub path: String,
    pub bytes: u64,
    pub human: String,
    /// `-wal` 的体量（VACUUM 之后它会被 checkpoint 收回去）。
    pub wal_bytes: u64,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsReposView {
    pub path: String,
    pub dirs: usize,
    pub bytes: u64,
    pub human: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsOverview {
    /// 本程序日志目录（engine.log 与轮转出来的 engine.log.N）。
    pub logs: OpsDirView,
    /// opencode 自己的日志（只展示，不在本面板清理——它由引擎侧按 8 MB 轮转）。
    pub engine_log: OpsDirView,
    pub database: OpsDbView,
    pub repos: OpsReposView,
    /// 会话库超过这个体量就在面板上标黄。
    pub db_warn_bytes: u64,
    /// engine.log 的轮转阈值，面板上说明用。
    pub log_rotate_at: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpsAction {
    pub removed: usize,
    pub freed_bytes: u64,
    pub freed_human: String,
    pub note: String,
}

fn dir_view(path: &Path) -> OpsDirView {
    let usage = ops_cleanup::dir_usage(path);
    OpsDirView {
        path: usage.path,
        files: usage.files,
        bytes: usage.bytes,
        human: ops_cleanup::human_bytes(usage.bytes),
    }
}

fn log_dir(app: &AppHandle) -> Result<PathBuf, StoryError> {
    app.path()
        .app_log_dir()
        .map_err(|e| StoryError::invalid("logDir", format!("拿不到日志目录：{e}")))
}

fn isolation(app: &AppHandle) -> Result<opencode_home::IsolationDirs, StoryError> {
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| StoryError::invalid("dataDir", format!("拿不到应用数据目录：{e}")))?;
    Ok(opencode_home::dirs_under(&base.join("opencode")))
}

fn repos_dir(dirs: &opencode_home::IsolationDirs) -> PathBuf {
    dirs.data.join("opencode").join("repos")
}

/// 三张卡片的数据源。只读，不建目录、不改文件。
#[tauri::command]
pub async fn ops_overview(app: AppHandle) -> Result<OpsOverview, StoryError> {
    let logs = dir_view(&log_dir(&app)?);
    let dirs = isolation(&app)?;
    let engine_log = dir_view(&dirs.opencode_log().parent().unwrap_or(&dirs.data));
    let db_path = dirs.db_path();
    let db_bytes = std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
    let wal_bytes = std::fs::metadata(db_path.with_extension("db-wal"))
        .map(|m| m.len())
        .unwrap_or(0);
    let repos_path = repos_dir(&dirs);
    let (repo_dirs, repo_bytes) = ops_cleanup::repos_usage(&repos_path);
    Ok(OpsOverview {
        logs,
        engine_log,
        database: OpsDbView {
            path: db_path.display().to_string(),
            bytes: db_bytes,
            human: ops_cleanup::human_bytes(db_bytes),
            wal_bytes,
            exists: db_path.is_file(),
        },
        repos: OpsReposView {
            path: repos_path.display().to_string(),
            dirs: repo_dirs,
            bytes: repo_bytes,
            human: ops_cleanup::human_bytes(repo_bytes),
        },
        db_warn_bytes: opencode_home::DB_WARN_BYTES,
        log_rotate_at: ops_cleanup::ENGINE_LOG_ROTATE_AT,
    })
}

/// 清本程序日志目录下的所有文件（目录保留，下一轮日志照旧写回）。
#[tauri::command]
pub async fn ops_clean_logs(app: AppHandle) -> Result<OpsAction, StoryError> {
    let dir = log_dir(&app)?;
    let (removed, freed) = ops_cleanup::clean_dir_files(&dir);
    let note = if removed == 0 {
        "日志目录里没有可清理的文件".to_string()
    } else {
        format!("已清理 {removed} 个日志文件")
    };
    engine::append_engine_log(
        &app,
        &format!(
            "运维：清理日志 {}（{removed} 个文件，释放 {}）",
            dir.display(),
            ops_cleanup::human_bytes(freed)
        ),
    );
    Ok(OpsAction {
        removed,
        freed_bytes: freed,
        freed_human: ops_cleanup::human_bytes(freed),
        note,
    })
}

/// 给会话库做整理：checkpoint → VACUUM → integrity_check。
///
/// 不做自动备份（按需求：要备份自己手动做），所以只做「整理」这一步，
/// 并且把 integrity_check 的结果原样带回界面。
#[tauri::command]
pub async fn ops_vacuum_database(app: AppHandle) -> Result<OpsAction, StoryError> {
    let dirs = isolation(&app)?;
    let db_path = dirs.db_path();
    if !db_path.is_file() {
        return Err(StoryError::invalid(
            "database",
            "会话库还不存在（本程序还没跑过一轮），不需要整理",
        ));
    }

    let (program, mut prefix) = python_program().ok_or_else(|| {
        StoryError::env_missing(
            "python",
            "整理会话库需要 python（带 sqlite3 模块）",
            "在 设置 → 环境自检 里看 python 是否就绪；或手动安装 python 3.10+ 后重试",
        )
    })?;
    prefix.push("-c".to_string());
    prefix.push(ops_cleanup::vacuum_script().to_string());
    prefix.push(db_path.display().to_string());

    let mut vacuum_cmd = std::process::Command::new(&program);
    vacuum_cmd.args(&prefix);
    engine::hide_console(&mut vacuum_cmd);
    let output = vacuum_cmd
        .output()
        .map_err(|e| StoryError::invalid("python", format!("调用 {program} 失败：{e}")))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        engine::append_engine_log(
            &app,
            &format!("运维：整理会话库失败（{}）：{detail}", db_path.display()),
        );
        return Err(StoryError::invalid(
            "database",
            format!(
                "整理会话库失败：{detail}；如果刚才有引擎在跑，等它结束后再试一次"
            ),
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    let before = parsed
        .get("before")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let after = parsed.get("after").and_then(|v| v.as_u64()).unwrap_or(0);
    let integrity = parsed
        .get("integrity")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let freed = before.saturating_sub(after);
    let note = format!(
        "整理完成：{} → {}（integrity_check {integrity}）",
        ops_cleanup::human_bytes(before),
        ops_cleanup::human_bytes(after)
    );
    engine::append_engine_log(
        &app,
        &format!("运维：整理会话库 {}，{note}", db_path.display()),
    );
    Ok(OpsAction {
        removed: 0,
        freed_bytes: freed,
        freed_human: ops_cleanup::human_bytes(freed),
        note,
    })
}

/// 删掉超过 `keep_days` 天没动过的 repos 子目录。
#[tauri::command]
pub async fn ops_clean_repos(app: AppHandle, keep_days: u32) -> Result<OpsAction, StoryError> {
    let dirs = isolation(&app)?;
    let root = repos_dir(&dirs);
    if !root.is_dir() {
        return Ok(OpsAction {
            removed: 0,
            freed_bytes: 0,
            freed_human: ops_cleanup::human_bytes(0),
            note: "repos 目录还不存在，没有可清理的仓库".to_string(),
        });
    }
    let now = SystemTime::now();
    let (removed, freed) = ops_cleanup::clean_old_repos(&root, keep_days, now);
    let note = if removed == 0 {
        format!("没有超过 {keep_days} 天没动过的仓库")
    } else {
        format!("已删除 {removed} 个超过 {keep_days} 天没动过的仓库")
    };
    engine::append_engine_log(
        &app,
        &format!(
            "运维：清理 repos（keep_days={keep_days}，{removed} 个目录，释放 {}）：{}",
            ops_cleanup::human_bytes(freed),
            root.display()
        ),
    );
    Ok(OpsAction {
        removed,
        freed_bytes: freed,
        freed_human: ops_cleanup::human_bytes(freed),
        note,
    })
}

/// 找 python：优先 PATH 上的 `python`，其次 `python3`、`py -3`。
fn python_program() -> Option<(String, Vec<String>)> {
    for (name, args) in [
        ("python", vec![]),
        ("python3", vec![]),
        ("py", vec!["-3".to_string()]),
    ] {
        let mut cmd = std::process::Command::new(name);
        cmd.args(&args)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // 探测 python 也要藏窗口：`py -3 --version` 这类调用同样会闪黑框。
        engine::hide_console(&mut cmd);
        if cmd.status().map(|s| s.success()).unwrap_or(false) {
            return Some((name.to_string(), args));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::ops_cleanup;

    #[test]
    fn vacuum_script_prints_one_json_line() {
        let script = ops_cleanup::vacuum_script();
        for key in ["wal_checkpoint(TRUNCATE)", "VACUUM", "integrity_check"] {
            assert!(script.contains(key), "脚本里必须有 {key}");
        }
        assert!(script.contains("json.dumps"), "结果要能被界面解析");
    }

    #[test]
    fn python_lookup_is_best_effort() {
        // 本机一定装了 python（gate 依赖它）；没有也不该 panic，只返回 None。
        if let Some((program, prefix)) = python_program() {
            assert!(!program.is_empty());
            assert!(prefix.len() <= 1);
        }
    }
}
