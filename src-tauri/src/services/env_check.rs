//! 环境自检：node(>=18) / python3 / bash(Git Bash) / 宿主 Agent CLI。
//!
//! 设计要点：
//! - 全部走 `Command` 探测，不依赖 PATH 一定正确：找不到时再按常见安装位置兜底；
//! - 任何一次探测都有超时上限（3s），到期 kill，绝不让 UI 卡死；
//! - Windows 下用 `CREATE_NO_WINDOW` 隐藏控制台窗口，避免每次自检闪黑框；
//! - 返回结构里保留 `path`：后续引擎启动时要把找到的绝对路径与它的目录塞进子进程 PATH。

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::models::dto::{EnvReport, ToolStatus};

const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

struct Probe {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

impl Probe {
    fn combined(&self) -> String {
        let mut s = String::new();
        if !self.stdout.trim().is_empty() {
            s.push_str(self.stdout.trim());
        }
        if !self.stderr.trim().is_empty() {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(self.stderr.trim());
        }
        s
    }
}

/// 运行 `program args...` 并抓取输出；超时/启动失败返回 None。
fn probe(program: &Path, args: &[&str]) -> Option<Probe> {
    probe_with_env(program, args, &[])
}

/// 与 `probe` 相同，但可以额外注入环境变量。
///
/// 环境自检会跑 `opencode --version` 取版本号；opencode 每次启动都会往自己的数据目录
/// 写日志（v2 还会 bootstrap 数据库），所以自检这一枪也要打在一次性的 XDG 目录里，
/// 不能写进用户真实的 `~/.local/share/opencode`。
fn probe_with_env(program: &Path, args: &[&str], envs: &[(&str, &str)]) -> Option<Probe> {
    let mut cmd = Command::new(program);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .envs(envs.iter().map(|(key, value)| (*key, *value)));
    hide_window(&mut cmd);

    let mut child = cmd.spawn().ok()?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                thread::sleep(Duration::from_millis(15));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }

    let out = child.wait_with_output().ok()?;
    Some(Probe {
        stdout: String::from_utf8_lossy(&out.stdout).to_string(),
        stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        code: out.status.code(),
    })
}

/// `which`：先按 PATH + PATHEXT 解析，支持传入含分隔符的绝对路径。
pub fn which_exe(name: &str) -> Option<PathBuf> {
    let as_path = Path::new(name);
    if name.contains('/') || name.contains('\\') {
        return if as_path.is_file() {
            Some(as_path.to_path_buf())
        } else {
            None
        };
    }

    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string())
            .split(';')
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };

    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        if dir.as_os_str().is_empty() {
            continue;
        }
        for ext in &exts {
            let candidate = dir.join(format!("{name}{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn first_line(text: &str) -> String {
    text.lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("")
        .to_string()
}

fn digits_of(text: &str) -> Option<String> {
    // 从 "v24.20.0" / "Python 3.14.6" / "GNU bash, version 5.2.37(1)-release" 里抓第一段数字
    let bytes: Vec<char> = text.chars().collect();
    let mut start = None;
    for (i, ch) in bytes.iter().enumerate() {
        if ch.is_ascii_digit() {
            start = Some(i);
            break;
        }
    }
    let start = start?;
    let mut end = start;
    while end < bytes.len() && (bytes[end].is_ascii_digit() || bytes[end] == '.') {
        end += 1;
    }
    let raw: String = bytes[start..end].iter().collect();
    Some(raw.trim_end_matches('.').to_string())
}

fn major_of(version: &str) -> Option<u32> {
    version.split('.').next()?.trim().parse::<u32>().ok()
}

// ---------------------------------------------------------------- node

fn check_node() -> ToolStatus {
    // PATH 优先；找不到认应用自动下载的安装目录（设置 → 环境自检 可一键下载）。
    let Some(path) = crate::services::runtime_setup::locate_node() else {
        return ToolStatus::missing(
            "node",
            "缺 Node.js：在 设置 → 环境自检 点「下载 Node.js」自动安装（国内镜像），或到 nodejs.org 手动安装 18+。",
            "PATH 与应用安装目录均未找到 node",
        );
    };
    let Some(out) = probe(&path, &["--version"]) else {
        return ToolStatus::missing(
            "node",
            "node 存在但无法执行，请重装 Node.js 18+。",
            &format!("{} --version 执行失败或超时", path.display()),
        );
    };
    let line = first_line(&out.combined());
    let version = digits_of(&line).unwrap_or_else(|| line.clone());
    let major = major_of(&version).unwrap_or(0);
    let ok = major >= 18;
    ToolStatus {
        tool: "node".into(),
        ok,
        version: Some(if line.is_empty() {
            version.clone()
        } else {
            line
        }),
        path: Some(path.display().to_string()),
        hint: if ok {
            None
        } else {
            Some("oh-story 要求 Node.js 18 或更高版本，请升级后重试。".into())
        },
        detail: Some(format!("node --version -> {}", out.combined())),
    }
}

// ---------------------------------------------------------------- python

fn check_python() -> ToolStatus {
    // 依次尝试 python / python3 / py -3；跳过 Windows 应用商店的空壳
    let attempts: [(&str, &[&str]); 3] = [
        ("python", &["--version"]),
        ("python3", &["--version"]),
        ("py", &["-3", "--version"]),
    ];

    let mut errors: Vec<String> = Vec::new();
    for (name, args) in attempts {
        let Some(path) = which_exe(name) else {
            errors.push(format!("{name}: 未找到"));
            continue;
        };
        let Some(out) = probe(&path, args) else {
            errors.push(format!("{name}: 执行失败或超时"));
            continue;
        };
        let text = out.combined();
        let line = first_line(&text);
        if out.code.unwrap_or(1) != 0 || !line.to_ascii_lowercase().contains("python") {
            // Windows 上未安装 Python 时 `python` 是商店占位程序，输出为空或提示去商店
            errors.push(format!("{name}: 无有效输出（{line}）"));
            continue;
        }
        let version = digits_of(&line).unwrap_or_default();
        let ok = major_of(&version).unwrap_or(0) == 3;
        return ToolStatus {
            tool: "python".into(),
            ok,
            version: Some(line.clone()),
            path: Some(path.display().to_string()),
            hint: if ok {
                None
            } else {
                Some("oh-story 的脚本需要 Python 3，请安装 Python 3.9+。".into())
            },
            detail: Some(format!("{} {} -> {}", path.display(), args.join(" "), line)),
        };
    }

    // PATH 全军覆没：认应用自动下载的嵌入式 Python（storyctl.py 只用标准库，够用）。
    if let Some(path) = crate::services::runtime_setup::locate_python() {
        if let Some(out) = probe(&path, &["--version"]) {
            let line = first_line(&out.combined());
            if line.to_ascii_lowercase().contains("python") {
                return ToolStatus {
                    tool: "python".into(),
                    ok: true,
                    version: Some(line.clone()),
                    path: Some(path.display().to_string()),
                    hint: None,
                    detail: Some(format!(
                        "{} --version -> {}（应用内置下载）",
                        path.display(),
                        line
                    )),
                };
            }
        }
    }

    ToolStatus::missing(
        "python",
        "缺 Python 3：在 设置 → 环境自检 点「下载 Python」自动安装（国内镜像，嵌入式免安装版），或到 python.org 手动安装 3.9+ 并勾选 Add to PATH。",
        &errors.join("; "),
    )
}

// ---------------------------------------------------------------- bash

fn is_wsl_or_system_bash(path: &Path) -> bool {
    path.to_string_lossy()
        .to_ascii_lowercase()
        .contains("\\windows\\system32\\bash.exe")
}

fn git_bash_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();

    // 0) 应用自动下载的 PortableGit（设置 → 环境自检 一键下载）
    if let Some(bash) = crate::services::runtime_setup::locate_gitbash() {
        out.push(bash);
    }

    // 由 git.exe 位置反推：<Git>\cmd\git.exe -> <Git>\bin\bash.exe
    if let Some(git) = which_exe("git") {
        if let Some(dir) = git.parent() {
            for rel in [
                "../bin/bash.exe",
                "../usr/bin/bash.exe",
                "../../bin/bash.exe",
            ] {
                out.push(dir.join(rel));
            }
        }
    }

    for key in [
        "ProgramFiles",
        "ProgramW6432",
        "ProgramFiles(x86)",
        "LOCALAPPDATA",
    ] {
        if let Ok(base) = std::env::var(key) {
            let base = PathBuf::from(base);
            out.push(base.join("Git/bin/bash.exe"));
            out.push(base.join("Git/usr/bin/bash.exe"));
            out.push(base.join("Programs/Git/bin/bash.exe"));
        }
    }
    out
}

fn check_bash() -> ToolStatus {
    // 1) PATH 里的 bash（可能是 Git Bash，也可能是 WSL 的 system32\bash.exe）
    if let Some(path) = which_exe("bash") {
        if !is_wsl_or_system_bash(&path) {
            if let Some(out) = probe(&path, &["--version"]) {
                let line = first_line(&out.combined());
                if line.to_ascii_lowercase().contains("bash") {
                    return ToolStatus {
                        tool: "bash".into(),
                        ok: true,
                        version: digits_of(&line).or(Some(line.clone())),
                        path: Some(path.display().to_string()),
                        hint: None,
                        detail: Some(format!("bash --version -> {line}")),
                    };
                }
            }
        }
    }

    // 2) 常见 Git for Windows 安装位置
    for candidate in git_bash_candidates() {
        if !candidate.is_file() {
            continue;
        }
        if let Some(out) = probe(&candidate, &["--version"]) {
            let line = first_line(&out.combined());
            if line.to_ascii_lowercase().contains("bash") {
                return ToolStatus {
                    tool: "bash".into(),
                    ok: true,
                    version: digits_of(&line).or(Some(line.clone())),
                    path: Some(candidate.display().to_string()),
                    hint: Some(
                        "bash 未在 PATH 中，已自动定位到 Git Bash；启动引擎时会把它的目录加入子进程 PATH。".into(),
                    ),
                    detail: Some(format!("{} --version -> {line}", candidate.display())),
                };
            }
        }
    }

    ToolStatus::missing(
        "bash",
        "安装 Git for Windows（含 Git Bash，https://git-scm.com/download/win），并把 `C:\\Program Files\\Git\\bin` 加入 PATH。oh-story 的 8 个 hook 都是 .sh，由 `bash` 执行。",
        "PATH 与常见安装目录均未找到可用的 Git Bash（WSL 的 system32\\bash.exe 不能用 Windows 路径跑 hook）。",
    )
}

// ---------------------------------------------------------------- 宿主 Agent CLI

fn check_engine() -> ToolStatus {
    let mut found: Vec<(String, String, String)> = Vec::new();
    let mut tried: Vec<String> = Vec::new();

    for name in ["claude", "codex"] {
        let Some(path) = which_exe(name) else {
            tried.push(format!("{name}: 未找到"));
            continue;
        };
        let Some(out) = probe(&path, &["--version"]) else {
            tried.push(format!("{name}: 执行失败或超时"));
            continue;
        };
        let line = first_line(&out.combined());
        found.push((
            name.to_string(),
            path.display().to_string(),
            if line.is_empty() {
                "版本未知".to_string()
            } else {
                line
            },
        ));
    }
    // opencode：PATH 里没有时再认应用自动下载的安装目录。
    let opencode = which_exe("opencode")
        .map(|p| (p, "PATH".to_string()))
        .or_else(|| {
            let exe = crate::services::opencode_setup::install_dir().join("opencode.exe");
            exe.is_file().then(|| (exe, "应用下载目录".to_string()))
        });
    match opencode {
        Some((path, _)) => {
            let probe_root = std::env::temp_dir().join("storydesk-opencode-probe");
            let (data, state, config) = (
                probe_root.join("data"),
                probe_root.join("state"),
                probe_root.join("config"),
            );
            let envs = [
                ("XDG_DATA_HOME", data.to_str().unwrap_or("")),
                ("XDG_STATE_HOME", state.to_str().unwrap_or("")),
                ("XDG_CONFIG_HOME", config.to_str().unwrap_or("")),
            ];
            if let Some(out) = probe_with_env(&path, &["--version"], &envs) {
                let line = first_line(&out.combined());
                found.push((
                    "opencode".to_string(),
                    path.display().to_string(),
                    if line.is_empty() {
                        "版本未知".to_string()
                    } else {
                        line
                    },
                ));
            } else {
                tried.push("opencode: 执行失败或超时".to_string());
            }
        }
        None => tried.push("opencode: 未找到".to_string()),
    }

    if found.is_empty() {
        return ToolStatus::missing(
            "engine",
            "安装宿主 Agent CLI（任一即可）：Claude Code `npm i -g @anthropic-ai/claude-code`；或 opencode——在 设置 → AI 引擎 点「下载 opencode」自动安装（自定义模型 API 推荐这条路）。",
            &tried.join("; "),
        );
    }

    let primary = &found[0];
    let all = found
        .iter()
        .map(|(n, p, v)| format!("{n} {v} ({p})"))
        .collect::<Vec<_>>()
        .join(" | ");

    ToolStatus {
        tool: "engine".into(),
        ok: true,
        version: Some(primary.2.clone()),
        path: Some(primary.1.clone()),
        hint: if found.len() > 1 {
            Some(format!("检测到多个宿主 CLI，当前默认使用 {}", primary.0))
        } else {
            None
        },
        detail: Some(all),
    }
}

// ---------------------------------------------------------------- 汇总

pub fn check_all() -> EnvReport {
    let node = check_node();
    let python = check_python();
    let bash = check_bash();
    let engine = check_engine();

    let all_ok = node.ok && python.ok && bash.ok;
    let engine_ok = engine.ok;
    let platform = format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH);
    let checked_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string());

    EnvReport {
        node,
        python,
        bash,
        engine,
        all_ok,
        engine_ok,
        platform,
        checked_at,
    }
}
