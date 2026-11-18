//! 文件监听：让前端知道「磁盘上的文件被别人改了」。
//!
//! 只监听一个目录——用户当前打开的项目工作区根，绝不监听应用自身源码目录。
//! 忽略了 `.storydesk`（含快照、垃圾桶、我们自己的临时文件）、`.story`（oh-story 内部区）、
//! `.git`、`target`、`node_modules`，所以自己写快照不会引起事件风暴。
//!
//! 事件合并：300ms 内到达的事件先攒着，静默 300ms 后一次性发 `fs://changed`，
//! 前端拿到的是「这一批变了哪些文件」，而不是每个 syscall 一条。

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tauri::{AppHandle, Emitter};

use crate::models::dto::FsChangedPayload;
use crate::models::StoryError;
use crate::services::atomic_write::TEMP_PREFIX;

/// 前端监听的事件名。
pub const EVENT_NAME: &str = "fs://changed";

/// 防抖窗口。
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// 命中任一段名就整条事件丢弃。
const IGNORE_DIRS: [&str; 5] = [".storydesk", ".story", ".git", "target", "node_modules"];

/// 监听器句柄：持有底层 watcher 与停止标志。
pub struct FsWatcher {
    watcher: Option<RecommendedWatcher>,
    stop: Option<Arc<AtomicBool>>,
    /// 保留监听根便于排查；当前无读取方，阶段 4 的引擎日志会用到。
    #[allow(dead_code)]
    root: Option<PathBuf>,
}

impl Default for FsWatcher {
    fn default() -> Self {
        Self::new()
    }
}

impl FsWatcher {
    pub fn new() -> Self {
        Self {
            watcher: None,
            stop: None,
            root: None,
        }
    }

    /// 开始监听 `root`（重复调用会先停掉旧的）。
    pub fn start(&mut self, app: AppHandle, root: &Path) -> Result<String, StoryError> {
        self.stop();

        let root = root.canonicalize().map_err(|e| {
            StoryError::fs(root.display().to_string(), format!("监听目录不可访问: {e}"))
        })?;

        let (tx, rx) = mpsc::channel::<notify::Result<Event>>();
        let mut watcher = notify::recommended_watcher(tx)
            .map_err(|e| StoryError::internal(format!("创建文件监听失败: {e}")))?;
        watcher
            .watch(&root, RecursiveMode::Recursive)
            .map_err(|e| StoryError::fs(root.display().to_string(), format!("监听失败: {e}")))?;

        let stop = Arc::new(AtomicBool::new(false));
        let stop_flag = Arc::clone(&stop);
        let watch_root = root.clone();

        std::thread::Builder::new()
            .name("storydesk-fs-watcher".into())
            .spawn(move || {
                let mut pending: BTreeSet<String> = BTreeSet::new();
                let mut kinds: BTreeSet<&'static str> = BTreeSet::new();

                loop {
                    if stop_flag.load(Ordering::Relaxed) {
                        break;
                    }
                    match rx.recv_timeout(DEBOUNCE) {
                        Ok(Ok(event)) => {
                            for path in &event.paths {
                                if is_ignored(path) {
                                    continue;
                                }
                                pending.insert(clean_path(path));
                            }
                            if !pending.is_empty() {
                                kinds.insert(kind_of(&event.kind));
                            }
                        }
                        // 单个事件错误（权限、消失）忽略即可，不该打断监听
                        Ok(Err(_)) => {}
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if pending.is_empty() {
                                continue;
                            }
                            let payload = FsChangedPayload {
                                root: clean_path(&watch_root),
                                paths: pending.iter().cloned().collect(),
                                kind: summarize(&kinds),
                            };
                            pending.clear();
                            kinds.clear();
                            let _ = app.emit(EVENT_NAME, payload);
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })
            .map_err(|e| StoryError::internal(format!("启动监听线程失败: {e}")))?;

        self.root = Some(root.clone());
        self.watcher = Some(watcher);
        self.stop = Some(stop);
        Ok(clean_path(&root))
    }

    /// 停止监听（幂等）。
    pub fn stop(&mut self) {
        if let Some(flag) = self.stop.take() {
            flag.store(true, Ordering::Relaxed);
        }
        // drop watcher 会关闭事件通道，监听线程随即退出
        self.watcher = None;
        self.root = None;
    }
}

impl Drop for FsWatcher {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Tauri 托管状态。
pub struct WatcherState(pub Mutex<FsWatcher>);

impl WatcherState {
    pub fn new() -> Self {
        Self(Mutex::new(FsWatcher::new()))
    }
}

impl Default for WatcherState {
    fn default() -> Self {
        Self::new()
    }
}

/// Windows 的 canonicalize 会产出 `\\?\` / `\\?\UNC\` 前缀；前端要拿路径跟工作区里的记录直接比对，
/// 所以发出去之前统一去掉，避免「同一个文件两种拼写」导致匹配失败。
fn clean_path(path: &Path) -> String {
    let raw = path.display().to_string();
    if let Some(rest) = raw.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = raw.strip_prefix(r"\\?\") {
        rest.to_string()
    } else {
        raw
    }
}

fn is_ignored(path: &Path) -> bool {
    for comp in path.components() {
        if let Component::Normal(part) = comp {
            let name = part.to_string_lossy();
            if IGNORE_DIRS.contains(&name.as_ref()) {
                return true;
            }
        }
    }
    match path.file_name() {
        Some(name) => {
            let name = name.to_string_lossy();
            name.starts_with(TEMP_PREFIX) || name.ends_with(".tmp") || name.ends_with(".swp")
        }
        None => false,
    }
}

fn kind_of(kind: &EventKind) -> &'static str {
    match kind {
        EventKind::Create(_) => "create",
        EventKind::Modify(_) => "modify",
        EventKind::Remove(_) => "remove",
        _ => "other",
    }
}

fn summarize(kinds: &BTreeSet<&'static str>) -> String {
    if kinds.len() == 1 {
        return kinds
            .iter()
            .next()
            .map(|s| s.to_string())
            .unwrap_or_default();
    }
    if kinds.is_empty() {
        return "other".to_string();
    }
    "mixed".to_string()
}
