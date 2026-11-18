//! opencode CLI 的定位与自动下载。
//!
//! - 优先找 PATH 里的 `opencode`；找不到再找本应用的安装目录
//!   `%LOCALAPPDATA%\storydesk\opencode\`（自动下载就装在这里）。
//! - 下载源：GitHub Releases 的固定资产 `opencode-windows-x64.zip`（约 78MB，
//!   解压即得原生 opencode.exe，不需要 node）。解压交给系统自带的
//!   PowerShell `Expand-Archive`，不为一个 zip 引一个新 crate。
//! - 下载进度通过 `opencode://progress` 事件推给前端（每 2MB 一条，防止事件刷屏）。

use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::models::StoryError;

/// 下载进度事件名。
pub const PROGRESS_EVENT: &str = "opencode://progress";

/// `opencode://progress` 的载荷。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeProgress {
    pub downloaded: u64,
    /// 响应头里拿得到才有（GitHub 大文件走重定向后一般有 Content-Length）
    pub total: Option<u64>,
    /// 阶段说明：「下载中」「解压中」…
    pub note: Option<String>,
}

/// opencode 的安装目录：`%LOCALAPPDATA%\storydesk\opencode\`。
pub fn install_dir() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
        .unwrap_or_else(std::env::temp_dir);
    base.join("storydesk").join("opencode")
}

/// 在 `dir` 里（最多两层）找 opencode.exe。zip 里 exe 在根上，但保险起见也扫一层子目录。
fn find_exe(dir: &Path) -> Option<PathBuf> {
    let root = dir.join("opencode.exe");
    if root.is_file() {
        return Some(root);
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return None;
    };
    for entry in entries.flatten() {
        let candidate = entry.path().join("opencode.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// 定位 opencode 可执行文件：PATH → 本应用安装目录。
pub fn locate() -> Option<PathBuf> {
    if let Some(p) = crate::services::env_check::which_exe("opencode") {
        return Some(p);
    }
    find_exe(&install_dir())
}

/// 带上「设置里指定的路径」的定位：指定路径 > PATH > 本应用安装目录。
///
/// 指定的路径必须是个真实存在的文件；写错就退回自动定位，不因为一个手写错
/// 让整个引擎起不来。
pub fn locate_preferring(custom: Option<&str>) -> Option<PathBuf> {
    if let Some(raw) = custom.map(str::trim).filter(|raw| !raw.is_empty()) {
        let path = PathBuf::from(raw);
        if path.is_file() {
            return Some(path);
        }
    }
    locate()
}

/// 应急回退二进制的固定路径（v1 那条，永远不自动删除）。
pub fn fallback_path() -> PathBuf {
    install_dir().join("opencode-v1.19.5.exe")
}

/// 若本地当前这条是 v1，就顺手留一份 `opencode-v1.19.5.exe` 当回退。
///
/// 只复制、不移动、不删除：升级到 v2 之后，用户仍能一键切回并跑通。
pub fn ensure_fallback_copy() -> Option<PathBuf> {
    let dest = fallback_path();
    if dest.is_file() {
        return Some(dest);
    }
    let current = locate()?;
    let version = crate::services::engine::probe_version(&current);
    let is_v1 = matches!(version, Some((major, _)) if major < 2);
    if !is_v1 {
        return None;
    }
    if std::fs::copy(&current, &dest).is_ok() {
        Some(dest)
    } else {
        None
    }
}

/// 健康自检失败时追加的一句：本地还留着 v1，可以切回去跑。
pub fn fallback_hint() -> Option<String> {
    let path = fallback_path();
    if path.is_file() {
        Some(format!(
            "本地还留着 v1.19.5（{}）：可在 设置 → AI 引擎 点「切回 v1.19.5」后重试。",
            path.display()
        ))
    } else {
        None
    }
}

fn emit_progress(app: &AppHandle, downloaded: u64, total: Option<u64>, note: Option<&str>) {
    let _ = app.emit(
        PROGRESS_EVENT,
        OpencodeProgress {
            downloaded,
            total,
            note: note.map(str::to_string),
        },
    );
}

/// 钉死的 opencode 版本。
///
/// 为什么不再用 `releases/latest`：v2 起 opencode 默认连「托管后台服务」并用固定端口，
/// 若自动下载拿到 2.x 而命令行还是 v1 的写法，就会和桌面版抢端口、报
/// 「Managed service port … is already in use」。版本必须可控，升级走显式改这个常量。
pub const PINNED_VERSION: &str = "2.2.0";

/// 下载源。GitHub 直连不通（墙）时自动换 ghproxy 镜像；两个都不行才报错。
pub fn url_github() -> String {
    format!(
        "https://github.com/sst/opencode/releases/download/v{PINNED_VERSION}/opencode-windows-x64.zip"
    )
}

pub fn url_mirror() -> String {
    format!("https://mirror.ghproxy.com/{}", url_github())
}

/// 已登记版本的 Windows x64 二进制 SHA-256（下载解压后对 `opencode.exe` 本体验证）。
///
/// 2.2.0 的值是实测填的：从 release 下 zip → 解压 → sha256(opencode.exe)。
/// 未登记的版本不静默放行：`install()` 会明确告诉用户「该版本未登记校验值」。
pub fn expected_sha256(version: &str) -> Option<&'static str> {
    match version {
        "2.2.0" => Some("dcb54dc811ac30a9107f2e704b2cf1ac29d52971d288499f0a2c7420e1504a48"),
        _ => None,
    }
}

/// 纯 Rust 的 SHA-256（项目里已有的 `sha1` crate 只算 SHA-1，不为一次校验引新依赖）。
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) =
            (h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7]);
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let t1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

/// 校验一个装好的二进制。返回 `None` 表示通过（或该版本未登记校验值，见 `note`）。
pub fn verify_binary(exe: &Path, version: &str) -> Result<Option<String>, StoryError> {
    let Some(expected) = expected_sha256(version) else {
        return Ok(Some(format!(
            "opencode 是 v{version}，该版本没有登记 SHA-256 校验值，本次安装未校验（如需校验请补登记）。"
        )));
    };
    let bytes = std::fs::read(exe).map_err(|e| {
        StoryError::internal(format!("读不到下载的 opencode（{}）：{e}", exe.display()))
    })?;
    let actual = sha256_hex(&bytes);
    if actual != expected {
        let _ = std::fs::remove_file(exe);
        return Err(StoryError::internal(format!(
            "下载的 opencode 校验失败，已删除不使用：期望 SHA-256 {expected}，实际 {actual}。             可能是镜像被篡改或网络中断导致文件损坏，请重试或手动下载后放到 {}。",
            install_dir().display()
        )));
    }
    Ok(None)
}

/// 探测本机代理：系统代理（注册表 Internet Settings）→ HTTP_PROXY/HTTPS_PROXY 环境变量。
///
/// 为什么需要：GitHub 在国内基本直连不通；应用里 reqwest 默认不读系统代理，
/// 用户浏览器能下载、应用不能下载的差异就在这里。
pub(crate) fn detect_proxy() -> Option<String> {
    #[cfg(windows)]
    {
        // Windows 系统代理注册表（「设置 → 网络 → 代理」配置的那个）
        if let Some(proxy) = system_proxy_from_registry() {
            return Some(proxy);
        }
    }
    // 环境变量兜底
    for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(value) = std::env::var(name) {
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// 读注册表的系统代理。格式可能是 `host:port` 或 `proto=host:port;...`（多协议），
/// 只取第一条 http/https 或裸 host:port。
#[cfg(windows)]
fn system_proxy_from_registry() -> Option<String> {
    let key = winreg::RegKey::predef(winreg::enums::HKEY_CURRENT_USER)
        .open_subkey(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings")
        .ok()?;
    let enable: u32 = key.get_value("ProxyEnable").ok()?;
    if enable != 1 {
        return None;
    }
    let server: String = key.get_value("ProxyServer").ok()?;
    let server = server.trim();
    if server.is_empty() {
        return None;
    }
    // `proto=host:port;proto=host:port` 形式：优先 http=/https=
    for pair in server.split(';') {
        if let Some(addr) = pair
            .strip_prefix("http=")
            .or_else(|| pair.strip_prefix("https="))
        {
            return Some(format!("http://{addr}"));
        }
    }
    // 裸 `host:port`
    if server.starts_with("http://") || server.starts_with("https://") {
        return Some(server.to_string());
    }
    Some(format!("http://{server}"))
}

/// 常见本地代理端口探测（Clash 7897/7890、v2rayN 10809 等）。
/// 系统代理和环境变量都没有时，扫一眼本机常见端口——用户多半开着代理软件
/// 但没勾「系统代理」，这是国内环境的常态。
fn probe_local_proxy() -> Option<String> {
    for port in [7897, 7890, 10808, 10809, 2080, 8080] {
        let addr = format!("127.0.0.1:{port}");
        if let Ok(socket) = addr.parse() {
            if std::net::TcpStream::connect_timeout(&socket, std::time::Duration::from_millis(300))
                .is_ok()
            {
                return Some(format!("http://{addr}"));
            }
        }
    }
    None
}

pub(crate) fn http_client(proxy: Option<&str>) -> Result<reqwest::Client, StoryError> {
    let mut builder =
        reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15));
    if let Some(proxy) = proxy {
        let parsed = reqwest::Proxy::all(proxy)
            .map_err(|e| StoryError::internal(format!("代理配置无效（{proxy}）：{e}")))?;
        builder = builder.proxy(parsed);
    }
    builder
        .build()
        .map_err(|e| StoryError::internal(format!("创建 HTTP 客户端失败：{e}")))
}

/// 依次尝试「直连 → 系统代理/本地端口代理 → 镜像」，拿到 2xx 就返回响应。
async fn fetch_zip(app: &AppHandle) -> Result<reqwest::Response, StoryError> {
    let proxy = detect_proxy().or_else(probe_local_proxy);
    let github = url_github();
    let mirror = url_mirror();
    let attempts: [(&str, &str, Option<String>); 3] = [
        ("GitHub 直连", github.as_str(), None),
        ("GitHub + 代理", github.as_str(), proxy.clone()),
        ("GitHub 镜像（ghproxy）", mirror.as_str(), proxy.clone()),
    ];

    let mut last_err = String::new();
    for (note, url, proxy) in attempts {
        // 同一个代理不试第二遍（第一次直连失败后，代理与镜像仍要试）
        emit_progress(app, 0, None, Some(&format!("正在尝试{note}…")));
        match http_client(proxy.as_deref())?.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                eprintln!("[storydesk] opencode 下载成功（{note}）");
                return Ok(resp);
            }
            Ok(resp) => last_err = format!("{note}：HTTP {}", resp.status()),
            Err(e) => last_err = format!("{note}：{e}"),
        }
    }
    Err(StoryError::internal(format!(
        "下载 opencode 失败（{last_err}）。你的网络访问 GitHub 不通：请开启代理软件后重试（应用会自动识别系统代理或常见本地端口），或手动到 github.com/sst/opencode/releases 下载 opencode-windows-x64.zip 解压到 {}",
        install_dir().display()
    )))
}

/// 自动下载并安装 opencode；已在就直接返回现有路径。
///
/// 返回 opencode.exe 的绝对路径（字符串给前端显示用）。
pub async fn install(app: &AppHandle) -> Result<String, StoryError> {
    if let Some(p) = locate() {
        return Ok(p.display().to_string());
    }

    let resp = fetch_zip(app).await?;
    let total = resp.content_length();
    let mut downloaded: u64 = 0;
    let mut last_emit: u64 = 0;
    // 78MB 的 zip 先攒在内存里再一次性落盘（避免引入 tokio fs / 新 crate）。
    let mut buf: Vec<u8> = Vec::with_capacity(total.unwrap_or(80 * 1024 * 1024) as usize);

    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| StoryError::internal(format!("下载 opencode 中断：{e}")))?
    {
        buf.extend_from_slice(&chunk);
        downloaded += chunk.len() as u64;
        if downloaded - last_emit >= 2 * 1024 * 1024 {
            last_emit = downloaded;
            emit_progress(app, downloaded, total, Some("下载中"));
        }
    }
    emit_progress(
        app,
        downloaded,
        Some(buf.len() as u64),
        Some("下载完成，正在解压…"),
    );

    let tmp = std::env::temp_dir().join(format!(
        "storydesk-opencode-{}.zip",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&tmp, &buf)
        .map_err(|e| StoryError::internal(format!("写临时文件失败（{}）：{e}", tmp.display())))?;

    let dir = install_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| StoryError::internal(format!("创建安装目录失败（{}）：{e}", dir.display())))?;

    // 系统自带 Expand-Archive 解压，不为一个 zip 引依赖。
    let script = format!(
        "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
        tmp.display().to_string().replace('\'', "''"),
        dir.display().to_string().replace('\'', "''"),
    );
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
            "解压 opencode 失败：{}",
            err.trim()
        )));
    }

    let exe = find_exe(&dir).ok_or_else(|| {
        StoryError::internal("解压完成但没找到 opencode.exe（zip 结构可能变了，请到 github.com/sst/opencode 手动安装）".to_string())
    })?;

    // 校验：钉死版本 + 对二进制本体算 SHA-256。不通过就删掉、绝不使用。
    emit_progress(app, downloaded, Some(buf.len() as u64), Some("正在校验 SHA-256…"));
    let note = verify_binary(&exe, PINNED_VERSION)?;
    if let Some(text) = &note {
        emit_progress(app, downloaded, Some(buf.len() as u64), Some(text));
        eprintln!("[storydesk] {text}");
    }
    emit_progress(app, downloaded, Some(buf.len() as u64), Some("安装完成"));
    Ok(exe.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 第 8 项 B：SHA-256 实现要跟标准测试向量对齐，否则校验等于没做。
    #[test]
    fn sha256_matches_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        // 跨越 64 字节分组边界
        assert_eq!(
            sha256_hex(&vec![b'a'; 1000]).len(),
            64,
            "长度固定 64 位十六进制"
        );
    }

    /// 下载源必须钉版本，不许再出现 `latest`（否则新装机器会拿到 v2 而参数还是 v1 的）。
    #[test]
    fn download_url_is_pinned_not_latest() {
        let github = url_github();
        assert!(!github.contains("/latest/"), "不许再用 latest：{github}");
        assert!(
            github.contains(&format!("/download/v{PINNED_VERSION}/")),
            "URL 应指向钉死的版本：{github}"
        );
        assert!(url_mirror().ends_with(&github), "镜像只是前缀包装");
        assert!(expected_sha256(PINNED_VERSION).is_some(), "钉死版本要登记校验值");
        assert!(expected_sha256("0.0.0").is_none());
    }

    /// 校验失败必须删掉文件并报错，不许「静默使用」。
    #[test]
    fn verify_binary_rejects_a_tampered_file() {
        let dir = std::env::temp_dir().join("storydesk-verify-sha");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("opencode.exe");
        std::fs::write(&exe, b"not the real binary").unwrap();

        let err = verify_binary(&exe, PINNED_VERSION).expect_err("哈希不符必须报错");
        let text = err.to_string();
        assert!(text.contains("校验失败"), "{text}");
        assert!(text.contains("已删除"), "{text}");
        assert!(!exe.exists(), "校验失败的文件要删掉");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 未登记的版本不拦，但要明确告诉用户「没校验」，不能假装校验过。
    #[test]
    fn unregistered_version_is_loud_but_allowed() {
        let dir = std::env::temp_dir().join("storydesk-verify-skip");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let exe = dir.join("opencode.exe");
        std::fs::write(&exe, b"whatever").unwrap();

        let note = verify_binary(&exe, "9.9.9").expect("未登记版本不拦");
        let note = note.expect("必须给出说明");
        assert!(note.contains("没有登记"), "{note}");
        assert!(exe.exists(), "跳过校验时不能删文件");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 设置里指定了路径就必须优先用它（本机升级 v2 的唯一通道）。
    #[test]
    fn configured_path_wins_over_auto_locate() {
        let dir = std::env::temp_dir().join("storydesk-configured-opencode");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("opencode.exe");
        std::fs::write(&fake, b"stub").unwrap();

        let picked = locate_preferring(Some(&fake.display().to_string()));
        assert_eq!(picked.as_deref(), Some(fake.as_path()));

        // 路径写错（不存在）→ 退回自动定位，而不是返回一个坏路径
        let missing = dir.join("nope.exe").display().to_string();
        let fallback = locate_preferring(Some(&missing));
        assert!(fallback.map(|p| p != std::path::Path::new(&missing)).unwrap_or(true));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fallback_binary_keeps_its_own_name() {
        assert!(fallback_path().ends_with("opencode-v1.19.5.exe"));
        assert_eq!(fallback_path().parent(), Some(install_dir().as_path()));
    }

    #[test]
    fn install_dir_is_under_localappdata() {
        let dir = install_dir();
        let text = dir.display().to_string().to_ascii_lowercase();
        assert!(text.contains("storydesk"), "安装目录应带 storydesk：{text}");
    }

    #[test]
    fn locate_never_panics() {
        // 只验证不炸：机器上有没有 opencode 都算过。
        let _ = locate();
    }
}
