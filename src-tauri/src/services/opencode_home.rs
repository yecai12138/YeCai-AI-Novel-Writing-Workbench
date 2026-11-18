//! opencode 的数据目录隔离。
//!
//! 起因：storydesk 自带的 opencode CLI 与桌面版 opencode 共用
//! `~/.local/share/opencode`——同一个 1.7 GB 的 `opencode.db`、同一个 log、同一个
//! `~/.config/opencode/opencode.json`（那里可能留着别处的 `model: tongling/gpt-4o`）。
//! 三者任意一处被另一个程序改动，本程序的表现就是「Unexpected server error」这种
//! 看不出原因的 500。
//!
//! 所以本模块给 storydesk 划出自己的一块地：
//!
//! ```text
//! <app_local_data>/opencode/
//!   data/    ← XDG_DATA_HOME  → opencode 把 db / log / repos 落在这里
//!   state/   ← XDG_STATE_HOME → locks / model.json / prompt-history.jsonl
//!   config/  ← XDG_CONFIG_HOME→ opencode 的全局配置（我们不用它，但要挡住外面的）
//!   log/     ← 本程序自己的运行日志（轮转）
//! ```
//!
//! 旧的 `~/.local/share/opencode` **一个字节都不碰**（只读都不读），桌面版的历史会话
//! 不再继承——这是隔离的既定代价，换来的是两边互不干扰。

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 隔离根目录下的四个子目录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationDirs {
    pub root: PathBuf,
    pub data: PathBuf,
    pub state: PathBuf,
    pub config: PathBuf,
    pub log: PathBuf,
}

impl IsolationDirs {
    /// opencode 自己的数据库（它固定放在 `$XDG_DATA_HOME/opencode/` 下）。
    pub fn db_path(&self) -> PathBuf {
        self.data.join("opencode").join("opencode.db")
    }

    /// opencode 自己的日志（同上，日志也归 DATA）。
    pub fn opencode_log(&self) -> PathBuf {
        self.data.join("opencode").join("log").join("opencode.log")
    }
}

/// 以 `root` 为隔离根，推出四个子目录（不创建）。
pub fn dirs_under(root: &Path) -> IsolationDirs {
    IsolationDirs {
        root: root.to_path_buf(),
        data: root.join("data"),
        state: root.join("state"),
        config: root.join("config"),
        log: root.join("log"),
    }
}

/// 建出四个目录；已存在就跳过。
pub fn ensure_dirs(dirs: &IsolationDirs) -> std::io::Result<()> {
    for dir in [&dirs.data, &dirs.state, &dirs.config, &dirs.log] {
        std::fs::create_dir_all(dir)?;
    }
    Ok(())
}

/// 注入子进程的环境变量。
///
/// 三个 XDG 变量一起给：只给 DATA 的话 opencode 仍会去读全局 config，那个
/// 幽灵 `model:` 还会继续干扰。macOS 上 opencode 也认这三枚变量，所以三个平台
/// 用同一套注入，不依赖任何系统默认值。
pub fn env_pairs(dirs: &IsolationDirs) -> Vec<(String, String)> {
    vec![
        (
            "XDG_DATA_HOME".to_string(),
            dirs.data.display().to_string(),
        ),
        (
            "XDG_STATE_HOME".to_string(),
            dirs.state.display().to_string(),
        ),
        (
            "XDG_CONFIG_HOME".to_string(),
            dirs.config.display().to_string(),
        ),
    ]
}

/// opencode 日志超过这个大小就轮转一次。
pub const ROTATE_AT: u64 = 8 * 1024 * 1024;

/// 轮转后保留几份历史。
pub const KEEP_COPIES: usize = 3;

/// 数据库超过这个大小就在界面告警（opencode 从不自己清理旧会话）。
pub const DB_WARN_BYTES: u64 = 512 * 1024 * 1024;

/// 日志超限就滚一份：`opencode.log` → `opencode.log.1`，旧的依次后移，超出份数的删掉。
///
/// 返回要告诉作者的一句话（没轮转就返回 `None`）。
pub fn rotate_log(path: &Path, max_bytes: u64, keep: usize) -> Option<String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size <= max_bytes || keep == 0 {
        return None;
    }
    // 最老的一份先删，否则 keep 份之外的会一直堆着。
    let _ = std::fs::remove_file(path.with_extension(format!("log.{keep}")));
    for index in (1..keep).rev() {
        let from = path.with_extension(format!("log.{index}"));
        let to = path.with_extension(format!("log.{}", index + 1));
        if from.exists() {
            let _ = std::fs::rename(&from, &to);
        }
    }
    let _ = std::fs::rename(path, path.with_extension("log.1"));
    Some(format!(
        "opencode 日志已轮转（原 {} MB，保留 {keep} 份）",
        size / 1024 / 1024
    ))
}

/// 数据库过大时给一句能照做的提示。
pub fn db_warning(path: &Path, threshold: u64) -> Option<String> {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if size <= threshold {
        return None;
    }
    Some(format!(
        "opencode 会话库已 {} MB（{}）。它只影响历史会话、不影响当前写作；\
         要腾空间可在 设置 → 引擎 里清理本程序的 opencode 数据目录。",
        size / 1024 / 1024,
        path.display()
    ))
}

/// 健康自检：跑一次 `opencode models`。
///
/// 这一步**不花 Token**（不调模型），却能同时验证：二进制能启动、隔离目录可写、
/// 配置能解析、provider 能注册。任何一环坏掉，这里立刻给出人话，
/// 而不是等界面抛一句 `Unexpected server error`。
///
/// 结果按可执行文件路径缓存：一个进程只探一次。
fn probe_once(program: &Path, envs: &[(String, String)]) -> Result<(), String> {
    static CACHE: Mutex<Option<(PathBuf, Result<(), String>)>> = Mutex::new(None);
    if let Ok(guard) = CACHE.lock() {
        if let Some((cached_path, cached)) = guard.as_ref() {
            if cached_path == program {
                return cached.clone();
            }
        }
    }

    let result = run_models(program, envs);

    if let Ok(mut guard) = CACHE.lock() {
        *guard = Some((program.to_path_buf(), result.clone()));
    }
    result
}

fn run_models(program: &Path, envs: &[(String, String)]) -> Result<(), String> {
    let mut cmd = std::process::Command::new(program);
    cmd.arg("models")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // v2 起 `models` 也会去连（或拉起）托管后台服务，固定端口被桌面版占着就报
    // 「Managed service port … is already in use」——自检必须带上同一个 `--standalone`。
    for flag in crate::services::engine::opencode_flags(crate::services::engine::probe_version(
        program,
    )) {
        cmd.arg(flag);
    }
    for (name, value) in envs {
        cmd.env(name, value);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }

    let mut child = cmd.spawn().map_err(|e| {
        format!(
            "opencode 启动不了（{}）：{e}。装到 %LOCALAPPDATA%\\storydesk\\opencode 或放进 PATH 后重试。",
            program.display()
        )
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let out_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Some(mut pipe) = stdout {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let err_reader = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        if let Some(mut pipe) = stderr {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "opencode 自检超时（>30 秒没响应）：{}",
                program.display()
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    };

    let stdout = String::from_utf8_lossy(&out_reader.join().unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&err_reader.join().unwrap_or_default()).to_string();

    match status {
        Some(status) if status.success() => {
            if stdout.trim().is_empty() {
                Err("opencode 自检没列出任何模型：配置可能没被读到（设置 → 模型 API 里检查提供商与模型 ID）".to_string())
            } else {
                Ok(())
            }
        }
        Some(status) => {
            let detail = stderr
                .lines()
                .rev()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("（没有输出）")
                .to_string();
            Err(format!(
                "opencode 自检失败（退出码 {}）：{detail}",
                status.code().unwrap_or(-1)
            ))
        }
        None => Err("opencode 自检没能拿到退出状态".to_string()),
    }
}

/// 派发前的健康自检入口。失败时返回可直接展示给作者的文案。
pub fn health_check(program: &Path, envs: &[(String, String)]) -> Result<(), String> {
    probe_once(program, envs).map_err(|detail| {
        format!("引擎自检没通过：{detail}。修好后再重新派发这一步（当前这一步没有调用模型）。")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isolation_keeps_three_xdg_roots_apart() {
        let root = std::env::temp_dir().join("storydesk-isolation-dirs");
        let _ = std::fs::remove_dir_all(&root);
        let dirs = dirs_under(&root);
        ensure_dirs(&dirs).expect("建目录");

        assert!(dirs.data.is_dir());
        assert!(dirs.state.is_dir());
        assert!(dirs.config.is_dir());
        assert!(dirs.log.is_dir());

        let envs: std::collections::HashMap<String, String> = env_pairs(&dirs).into_iter().collect();
        assert_eq!(envs.get("XDG_DATA_HOME"), Some(&dirs.data.display().to_string()));
        assert_eq!(
            envs.get("XDG_STATE_HOME"),
            Some(&dirs.state.display().to_string())
        );
        assert_eq!(
            envs.get("XDG_CONFIG_HOME"),
            Some(&dirs.config.display().to_string())
        );
        // 数据库与日志都归 DATA，不在 STATE——opencode 自己的约定。
        assert!(dirs.db_path().starts_with(&dirs.data));
        assert!(dirs.opencode_log().starts_with(&dirs.data));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn oversized_log_is_rotated_and_old_copies_are_dropped() {
        let root = std::env::temp_dir().join("storydesk-isolation-rotate");
        let _ = std::fs::remove_dir_all(&root);
        let dirs = dirs_under(&root);
        ensure_dirs(&dirs).expect("建目录");
        let log = dirs.opencode_log();
        std::fs::create_dir_all(log.parent().unwrap()).unwrap();
        std::fs::write(&log, "x".repeat(2048)).unwrap();

        let note = rotate_log(&log, 1024, 2).expect("应该轮转");
        assert!(note.contains("轮转"));
        assert!(!log.exists());
        assert!(log.with_extension("log.1").exists());

        // 再轮一次：旧的 .1 变成 .2，新的顶上，超过 keep 的删掉。
        std::fs::write(&log, "y".repeat(2048)).unwrap();
        assert!(rotate_log(&log, 1024, 2).is_some());
        assert!(log.with_extension("log.2").exists());
        assert!(!log.with_extension("log.3").exists());

        // 没超限就不动。
        assert!(rotate_log(&log, 8 * 1024 * 1024, 3).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn db_warning_only_fires_above_the_threshold() {
        let root = std::env::temp_dir().join("storydesk-isolation-db");
        let _ = std::fs::remove_dir_all(&root);
        let dirs = dirs_under(&root);
        ensure_dirs(&dirs).expect("建目录");
        let db = dirs.db_path();
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        std::fs::write(&db, b"small").unwrap();
        assert!(db_warning(&db, 1024).is_none());
        assert!(db_warning(&db, 2).is_some());
        // 文件不存在时也不该告警（第一次跑本来就没有库）。
        assert!(db_warning(&root.join("missing.db"), 1).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }
}
