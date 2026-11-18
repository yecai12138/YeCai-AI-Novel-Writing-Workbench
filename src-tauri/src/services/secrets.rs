//! API Key 保管：走系统凭据库（Windows Credential Manager / macOS Keychain / Secret Service）。
//!
//! 硬规则（阶段 5 起按「多提供商」组织）：
//! - 凭据库的 **account 名 = providerId**，同一个服务名下可以有多个 Key；
//! - key 只在 Rust 侧读取，**永远不回传前端**（前端只拿 `hasKey: boolean`）；
//! - key 不进命令行参数（会出现在进程列表里），只经 `envs()` 注入子进程环境；
//! - 不写日志、不写 `settings.json`、不写 `.storydesk/`、不进快照。

use keyring::Entry;

use crate::models::StoryError;

/// 凭据库里的服务名（固定，改了就取不到旧 key）。
const SERVICE: &str = "storydesk";
/// 阶段 4 的单 Key 账户名，仅用于一次性迁移。
const LEGACY_ANTHROPIC_ACCOUNT: &str = "anthropic-api-key";

/// keyring 在本机实际用的后端名。
pub fn backend_name() -> &'static str {
    if cfg!(windows) {
        "windows-native"
    } else if cfg!(target_os = "macos") {
        "keychain"
    } else {
        "secret-service"
    }
}

fn entry(provider_id: &str) -> Result<Entry, StoryError> {
    if provider_id.trim().is_empty() {
        return Err(StoryError::invalid("providerId", "providerId 不能为空"));
    }
    Entry::new(SERVICE, provider_id)
        .map_err(|e| StoryError::internal(format!("无法访问系统凭据库（{}）: {e}", backend_name())))
}

/// 写入某个提供商的 Key；空字符串视为删除。返回是否已存在（true = 已存）。
pub fn save_provider_key(provider_id: &str, key: &str) -> Result<bool, StoryError> {
    let trimmed = key.trim();
    if trimmed.is_empty() {
        delete_provider_key(provider_id)?;
        return Ok(false);
    }
    entry(provider_id)?
        .set_password(trimmed)
        .map_err(|e| StoryError::internal(format!("写入系统凭据库失败: {e}")))?;
    Ok(true)
}

/// 取某个提供商的 Key（仅供引擎注入环境变量，不要在命令返回值里用它）。
pub fn get_provider_key(provider_id: &str) -> Option<String> {
    entry(provider_id)
        .ok()?
        .get_password()
        .ok()
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
}

/// 是否有 Key（不读内容、不回传内容）。
pub fn has_provider_key(provider_id: &str) -> bool {
    get_provider_key(provider_id).is_some()
}

/// 删除某个提供商的 Key（不存在也算成功）。
pub fn delete_provider_key(provider_id: &str) -> Result<(), StoryError> {
    match entry(provider_id)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(StoryError::internal(format!("删除凭据失败: {e}"))),
    }
}

/// 阶段 4 的旧 Key（账户名 `anthropic-api-key`）迁到 `anthropic` 提供商名下。
///
/// 只在目标账户还没有 Key 时才动，失败静默——迁移是尽力而为，不能挡住启动。
pub fn migrate_legacy_anthropic_key() {
    if has_provider_key("anthropic") {
        return;
    }
    let Ok(legacy) = Entry::new(SERVICE, LEGACY_ANTHROPIC_ACCOUNT) else {
        return;
    };
    let Ok(pw) = legacy.get_password() else {
        return;
    };
    if pw.trim().is_empty() {
        return;
    }
    if save_provider_key("anthropic", &pw).unwrap_or(false) {
        let _ = legacy.delete_credential();
    }
}
