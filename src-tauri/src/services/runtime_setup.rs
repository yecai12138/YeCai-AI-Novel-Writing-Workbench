//! 运行时依赖（Node.js / Python / Git Bash）的国内镜像自动下载。
//!
//! 为什么走 npmmirror / 华为云镜像而不是官网：官网（nodejs.org / python.org /
//! gitforwindows.org）国内直连慢或不通；npmmirror（阿里）与华为云的镜像直连快。
//! 两个镜像互为兜底，全部失败才报错。
//!
//! 安装位置：`%LOCALAPPDATA%\storydesk\<tool>\`，与 opencode 同一套约定。
//! 所有工具都是「解压即用」，不需要安装器、不写注册表、不污染系统 PATH——
//! 应用内的探测函数（env_check / gate / engine）在 PATH 找不到时会认这个目录。

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::models::StoryError;

/// 下载进度事件名（与 opencode://progress 分开，载荷多了 tool 字段）。
pub const PROGRESS_EVENT: &str = "setup://progress";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeTool {
    Node,
    Python,
    GitBash,
}

impl RuntimeTool {
    pub fn parse(id: &str) -> Option<Self> {
        match id {
            "node" => Some(Self::Node),
            "python" => Some(Self::Python),
            "gitbash" => Some(Self::GitBash),
            _ => None,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Self::Node => "node",
            Self::Python => "python",
            Self::GitBash => "gitbash",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Node => "Node.js",
            Self::Python => "Python",
            Self::GitBash => "Git Bash",
        }
    }

    /// 两个国内镜像（npmmirror 主、华为云备）。镜像挂了自动换。
    fn urls(self) -> [(&'static str, &'static str); 2] {
        match self {
            // Node 22 LTS（storyctl 链不依赖 node，但 hook 生态和宿主 CLI 常要它）
            Self::Node => [
                ("npmmirror", "https://registry.npmmirror.com/-/binary/node/v22.14.0/node-v22.14.0-win-x64.zip"),
                ("huaweicloud", "https://mirrors.huaweicloud.com/nodejs/v22.14.0/node-v22.14.0-win-x64.zip"),
            ],
            // Python 嵌入版：免安装、解压即用，标准库齐全（storyctl.py 只用标准库）
            Self::Python => [
                ("npmmirror", "https://registry.npmmirror.com/-/binary/python/3.12.9/python-3.12.9-embed-amd64.zip"),
                ("huaweicloud", "https://mirrors.huaweicloud.com/python/3.12.9/python-3.12.9-embed-amd64.zip"),
            ],
            // PortableGit：自带 bash + coreutils（hook 是 .sh，要完整 Git Bash 环境）
            Self::GitBash => [
                ("npmmirror", "https://registry.npmmirror.com/-/binary/git-for-windows/v2.47.1.windows.1/PortableGit-2.47.1-64-bit.7z.exe"),
                ("huaweicloud", "https://mirrors.huaweicloud.com/git-for-windows/v2.47.1.windows.1/PortableGit-2.47.1-64-bit.7z.exe"),
            ],
        }
    }

    /// 安装目录：`%LOCALAPPDATA%\storydesk\<id>\`。
    pub fn install_dir(self) -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_else(std::env::temp_dir);
        base.join("storydesk").join(self.id())
    }

    /// 安装后的关键可执行文件（验证安装是否成功）。
    fn exe_path(self) -> PathBuf {
        let dir = self.install_dir();
        match self {
            // node zip 解压后是 node-v22.14.0-win-x64\node.exe
            Self::Node => dir.join("node-v22.14.0-win-x64").join("node.exe"),
            // python embed 解压后 python.exe 在根
            Self::Python => dir.join("python.exe"),
            // PortableGit 解压后优先用 usr\bin\bash.exe（同目录就是 coreutils，
            // PATH 前置它的 parent 即可让 hook 找到全部工具）
            Self::GitBash => dir.join("usr").join("bin").join("bash.exe"),
        }
    }

    /// exe 实际所在目录（定位用；node 可能带版本子目录，扫一层兜底）。
    fn locate_in(self, dir: &Path) -> Option<PathBuf> {
        let direct = self.exe_path();
        if direct.is_file() {
            return Some(direct);
        }
        // 兜底：在安装目录里（最多两层）找同名 exe
        let exe_name = match self {
            Self::Node => "node.exe",
            Self::Python => "python.exe",
            Self::GitBash => "bash.exe",
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return None;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let candidate = path.join(exe_name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            } else if path.file_name().and_then(|n| n.to_str()) == Some(exe_name) {
                return Some(path);
            }
        }
        None
    }
}

/// 定位三个工具（PATH 优先，找不到认应用安装目录）。
pub fn locate_node() -> Option<PathBuf> {
    crate::services::env_check::which_exe("node")
        .or_else(|| RuntimeTool::Node.locate_in(&RuntimeTool::Node.install_dir()))
}

pub fn locate_python() -> Option<PathBuf> {
    crate::services::env_check::which_exe("python")
        .or_else(|| RuntimeTool::Python.locate_in(&RuntimeTool::Python.install_dir()))
}

/// 定位 Git Bash 的 bash.exe（PATH 里非 system32 的 → 应用安装目录）。
pub fn locate_gitbash() -> Option<PathBuf> {
    crate::services::env_check::which_exe("bash")
        .filter(|p| {
            !p.to_string_lossy()
                .to_ascii_lowercase()
                .contains("system32")
        })
        .or_else(|| RuntimeTool::GitBash.locate_in(&RuntimeTool::GitBash.install_dir()))
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SetupProgress {
    tool: String,
    downloaded: u64,
    total: Option<u64>,
    note: Option<String>,
}

fn emit_progress(
    app: &AppHandle,
    tool: RuntimeTool,
    downloaded: u64,
    total: Option<u64>,
    note: Option<&str>,
) {
    let _ = app.emit(
        PROGRESS_EVENT,
        SetupProgress {
            tool: tool.id().to_string(),
            downloaded,
            total,
            note: note.map(str::to_string),
        },
    );
}

/// 下载并安装指定工具；已安装直接返回 exe 路径。
pub async fn install(tool: RuntimeTool, app: &AppHandle) -> Result<String, StoryError> {
    if let Some(p) = tool.locate_in(&tool.install_dir()) {
        return Ok(p.display().to_string());
    }

    // 下载（npmmirror 直连 → 华为云直连 → npmmirror + 代理）
    let proxy = crate::services::opencode_setup::detect_proxy();
    let mut last_err = String::new();
    let mut archive: Option<(Vec<u8>, &str)> = None;
    for (mirror, url) in tool.urls() {
        emit_progress(app, tool, 0, None, Some(&format!("正在从{mirror}下载…")));
        match download(app, tool, url, None).await {
            Ok(bytes) => {
                archive = Some((bytes, mirror));
                break;
            }
            Err(e) => last_err = format!("{mirror}：{e}"),
        }
    }
    let (bytes, _mirror) = match archive {
        Some(v) => v,
        None => {
            // 两个镜像直连都失败：带代理再试一轮（npmmirror）
            let (mirror, url) = tool.urls()[0];
            emit_progress(
                app,
                tool,
                0,
                None,
                Some(&format!("{mirror}直连失败，尝试代理…")),
            );
            match download(app, tool, url, proxy.as_deref()).await {
                Ok(bytes) => (bytes, mirror),
                Err(e) => {
                    let msg = format!(
                        "下载 {} 失败（{last_err}；代理重试：{e}）。请检查网络，或手动安装 {} 后重试",
                        tool.label(),
                        tool.label(),
                    );
                    return Err(StoryError::internal(msg));
                }
            }
        }
    };

    // 解压：zip 用 PowerShell Expand-Archive；PortableGit 是 7z SFX，自解压静默参数
    let dir = tool.install_dir();
    emit_progress(
        app,
        tool,
        bytes.len() as u64,
        Some(bytes.len() as u64),
        Some("下载完成，正在解压…"),
    );
    std::fs::create_dir_all(&dir)
        .map_err(|e| StoryError::internal(format!("创建安装目录失败（{}）：{e}", dir.display())))?;

    let tmp = std::env::temp_dir().join(format!(
        "storydesk-{}-{}.{}",
        tool.id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
        if tool == RuntimeTool::GitBash {
            "exe"
        } else {
            "zip"
        }
    ));
    std::fs::write(&tmp, &bytes)
        .map_err(|e| StoryError::internal(format!("写临时文件失败（{}）：{e}", tmp.display())))?;

    let script = if tool == RuntimeTool::GitBash {
        // PortableGit SFX：`<sfx> -y -o<dir>` 静默自解压（Git for Windows 官方用法）
        format!(
            "Start-Process -FilePath '{}' -ArgumentList '-y','-o{}' -Wait -NoNewWindow",
            tmp.display().to_string().replace('\'', "''"),
            dir.display().to_string().replace('\'', "''"),
        )
    } else {
        format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            tmp.display().to_string().replace('\'', "''"),
            dir.display().to_string().replace('\'', "''"),
        )
    };

    let mut cmd = std::process::Command::new("powershell");
    cmd.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .map_err(|e| StoryError::internal(format!("启动 PowerShell 解压失败：{e}")))?;
    let _ = std::fs::remove_file(&tmp);
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(StoryError::internal(format!(
            "解压 {} 失败：{}",
            tool.label(),
            err.trim()
        )));
    }

    let exe = tool.locate_in(&dir).ok_or_else(|| {
        StoryError::internal(format!(
            "{} 解压完成但没找到可执行文件（安装目录：{}）",
            tool.label(),
            dir.display()
        ))
    })?;
    emit_progress(
        app,
        tool,
        bytes.len() as u64,
        Some(bytes.len() as u64),
        Some("安装完成"),
    );
    Ok(exe.display().to_string())
}

/// 下载一个 URL 到内存（进度每 2MB 推一次）。`proxy` 为 None 时不走代理。
async fn download(
    app: &AppHandle,
    tool: RuntimeTool,
    url: &str,
    proxy: Option<&str>,
) -> Result<Vec<u8>, StoryError> {
    let client = crate::services::opencode_setup::http_client(proxy)?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| StoryError::internal(format!("{e}")))?;
    if !resp.status().is_success() {
        return Err(StoryError::internal(format!("HTTP {}", resp.status())));
    }
    let total = resp.content_length();
    let mut downloaded: u64 = 0;
    let mut last_emit: u64 = 0;
    let mut buf: Vec<u8> = Vec::with_capacity(total.unwrap_or(64 * 1024 * 1024) as usize);
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| StoryError::internal(format!("下载中断：{e}")))?
    {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded - last_emit >= 2 * 1024 * 1024 {
            last_emit = downloaded;
            emit_progress(app, tool, downloaded, total, Some("下载中"));
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_and_roundtrip() {
        for id in ["node", "python", "gitbash"] {
            assert_eq!(RuntimeTool::parse(id).unwrap().id(), id);
        }
        assert!(RuntimeTool::parse("claude").is_none());
    }

    #[test]
    fn install_dirs_are_separated() {
        let a = RuntimeTool::Node.install_dir();
        let b = RuntimeTool::Python.install_dir();
        let c = RuntimeTool::GitBash.install_dir();
        assert_ne!(a, b);
        assert_ne!(b, c);
    }

    #[test]
    fn urls_are_domestic_mirrors() {
        for tool in [RuntimeTool::Node, RuntimeTool::Python, RuntimeTool::GitBash] {
            for (mirror, url) in tool.urls() {
                assert!(
                    url.starts_with("https://registry.npmmirror.com")
                        || url.starts_with("https://mirrors.huaweicloud.com"),
                    "{mirror} 镜像地址不对：{url}"
                );
            }
        }
    }
}
