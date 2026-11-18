//! 全局设置的读写：`<appConfigDir>/settings.json`。
//!
//! - 文件里**只有非敏感配置**（providers / activeProviderId / engine / autosave），
//!   任何 API Key 都只存系统凭据库，绝不落到这里；
//! - 原子写复用 `atomic_write`（临时文件 → fsync → rename），避免半截文件；
//! - 读失败（不存在 / JSON 坏了）一律回落默认值，不让应用起不来。

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};

use crate::models::settings::{AppSettings, ProviderConfig, ProviderKeyState, SettingsView};
use crate::models::StoryError;
use crate::services::{atomic_write, secrets, snapshot};

/// 设置文件名。
pub const SETTINGS_FILE: &str = "settings.json";

/// app config dir 下的 settings.json 路径（Windows: `%APPDATA%\<identifier>\settings.json`）。
pub fn settings_path(app: &AppHandle) -> Result<PathBuf, StoryError> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| StoryError::internal(format!("拿不到应用配置目录: {e}")))?;
    Ok(dir.join(SETTINGS_FILE))
}

/// 纯函数版：从某个路径读设置（读不到或解析失败都返回 None，由调用方回落默认值）。
/// 读设置文本：统一走 atomic_write::read_text（它会剥掉 UTF-8 BOM）。
///
/// 以前是 std::fs::read_to_string + from_str(..).ok()：记事本存一次「UTF-8 with BOM」，
/// serde_json 就判成坏 JSON，load() 回落全默认，紧接着任何一次 save 都会把 providers /
/// activeProviderId 整份覆盖掉——用户只看到「配置自己没了」。
/// Ok(None) 只代表「文件不存在」；读不出来（非 UTF-8 / IO 错）是 Err。
fn read_settings_text(path: &Path) -> Result<Option<String>, StoryError> {
    if !path.exists() {
        return Ok(None);
    }
    atomic_write::read_text(path).map(Some)
}

/// 纯函数版：从某个路径读设置（读不到或解析失败返回 None，由调用方回落默认值）。
///
/// 返回 None 只表示「这次读不到」，**不代表可以拿默认值覆盖磁盘**：覆盖前一律走 save_to，
/// 它会先把无法解析的原文件带时间戳备份出来，备份失败就不写。
pub fn load_from(path: &Path) -> Option<AppSettings> {
    let text = read_settings_text(path).ok().flatten()?;
    serde_json::from_str::<AppSettings>(&text).ok()
}

/// 磁盘上的设置如果解析不了，先带时间戳备份一份，返回备份路径。
fn backup_unparsable(path: &Path) -> Result<Option<PathBuf>, StoryError> {
    let Some(text) = read_settings_text(path)? else {
        return Ok(None);
    };
    if serde_json::from_str::<AppSettings>(&text).is_ok() {
        return Ok(None);
    }
    let backup = path.with_extension(format!("json.broken-{}.bak", snapshot::iso_now()));
    std::fs::copy(path, &backup).map_err(|e| {
        StoryError::fs(
            backup.display().to_string(),
            format!("配置文件解析失败、备份也失败，已放弃写入：{e}"),
        )
    })?;
    Ok(Some(backup))
}

/// 纯函数版：写到某个路径（自动建父目录）。
///
/// 返回 Ok(Some(备份路径)) 表示「磁盘上原文件解析不了，已先备份再写」——这条信息要往上
/// 交到用户面前（见 save）。备份失败直接 Err，绝不带着「反正写下去也能用」的侥幸覆盖。
pub fn save_to(path: &Path, settings: &AppSettings) -> Result<Option<PathBuf>, StoryError> {
    let backup = backup_unparsable(path)?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| {
            StoryError::fs(format!("创建配置目录失败: {e}"), dir.display().to_string())
        })?;
    }
    let body = serde_json::to_vec_pretty(settings)
        .map_err(|e| StoryError::internal(format!("序列化设置失败: {e}")))?;
    atomic_write::write_atomic(path, &body)?;
    Ok(backup)
}

/// 读设置（含清洗与旧 Key 迁移）。
pub fn load(app: &AppHandle) -> AppSettings {
    let mut settings = settings_path(app)
        .ok()
        .and_then(|p| load_from(&p))
        .unwrap_or_default();
    // 首次补齐内置提供商：用户新装的版本会多出新的内置项，但不覆盖用户已有的，
    // 也不补回用户主动删掉的（`removed_builtins`）。
    let mut seed = AppSettings::default();
    seed.providers.retain(|p| {
        settings.provider(&p.id).is_none()
            && !settings.removed_builtins.iter().any(|id| id == &p.id)
    });
    if !seed.providers.is_empty() {
        settings.providers.extend(seed.providers);
    }
    settings.normalize();
    secrets::migrate_legacy_anthropic_key();
    settings
}

/// 写设置并回读视图。
pub fn save(app: &AppHandle, mut settings: AppSettings) -> Result<SettingsView, StoryError> {
    settings.normalize();
    let path = settings_path(app)?;
    if let Some(backup) = save_to(&path, &settings)? {
        // 原文件解析不了（手改坏了 / 带了 BOM / 尾逗号），已经另存一份再覆盖。
        // 落一条引擎日志，用户事后能在日志里看到东西还在。
        crate::services::engine::append_engine_log(
            app,
            &format!(
                "设置文件无法解析，已备份为 {} 后重新写入",
                backup.display()
            ),
        );
    }
    view(app)
}

/// 前端视图：设置 + 每个提供商是否有 Key + 文件路径。
pub fn view(app: &AppHandle) -> Result<SettingsView, StoryError> {
    let settings = load(app);
    let path = settings_path(app)?;
    Ok(SettingsView {
        provider_keys: settings
            .providers
            .iter()
            .map(|p| ProviderKeyState {
                provider_id: p.id.clone(),
                has_key: secrets::has_provider_key(&p.id),
                supports_env: p.supports_env(),
                key_env: p.api_key_env().to_string(),
            })
            .collect(),
        settings,
        path: crate::services::workspace::strip_verbatim(path)
            .display()
            .to_string(),
        secret_backend: secrets::backend_name().to_string(),
    })
}

/// 当前的激活提供商 + 它的 Key（None 表示没配 Key，调用方不要注入空变量）。
pub fn active_provider_with_key(app: &AppHandle) -> Option<(ProviderConfig, Option<String>)> {
    let settings = load(app);
    let provider = settings.active_provider()?.clone();
    let key = secrets::get_provider_key(&provider.id);
    Some((provider, key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::settings::builtin_providers;

    #[test]
    fn defaults_have_four_providers_and_active_anthropic() {
        let s = AppSettings::default();
        assert_eq!(s.providers.len(), builtin_providers().len());
        assert_eq!(s.active_provider_id.as_deref(), Some("anthropic"));
        assert!(s.active_provider().is_some());
    }

    #[test]
    fn normalize_drops_duplicates_and_fixes_dangling_active() {
        let mut s = AppSettings::default();
        s.providers.push(s.providers[0].clone()); // 重复 id
        s.active_provider_id = Some("ghost".to_string());
        s.normalize();
        let ids: Vec<&str> = s.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids.iter().filter(|i| **i == "openai").count(), 1);
        // 悬空的 active 不能回落到第一个：删除当前提供商后必须保持「没有当前」
        assert_eq!(s.active_provider_id, None);
    }

    #[test]
    fn removed_builtins_survive_normalize_but_drop_unknown() {
        let mut s = AppSettings::default();
        s.removed_builtins = vec![
            "bedrock".to_string(),
            "bedrock".to_string(),
            "not-a-builtin".to_string(),
        ];
        s.normalize();
        assert_eq!(s.removed_builtins, vec!["bedrock".to_string()]);
        assert!(AppSettings::is_builtin("moonshot"));
        assert!(!AppSettings::is_builtin("custom-acme"));
    }

    #[test]
    fn protocol_maps_to_env_names() {
        let s = AppSettings::default();
        let anthropic = s.provider("anthropic").unwrap();
        assert_eq!(anthropic.api_key_env(), "ANTHROPIC_API_KEY");
        assert_eq!(anthropic.base_url_env(), Some("ANTHROPIC_BASE_URL"));
        let openai = s.provider("openai").unwrap();
        assert_eq!(openai.api_key_env(), "OPENAI_API_KEY");
        assert_eq!(openai.base_url_env(), Some("OPENAI_BASE_URL"));
        let bedrock = s.provider("bedrock").unwrap();
        assert_eq!(bedrock.api_key_env(), "AWS_BEARER_TOKEN_BEDROCK");
        assert_eq!(bedrock.base_url_env(), None);
        assert!(bedrock.supports_env());
    }

    #[test]
    fn roundtrip_through_disk() {
        let dir = std::env::temp_dir().join("storydesk-settings-test");
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("settings.json");
        let mut s = AppSettings::default();
        s.providers.push(ProviderConfig {
            id: "custom-acme".to_string(),
            name: "Acme 中转".to_string(),
            base_url: "https://acme.example/v1".to_string(),
            protocol: "openai".to_string(),
            models: vec!["acme-1".to_string()],
            active_model: None,
        });
        s.active_provider_id = Some("custom-acme".to_string());
        save_to(&path, &s).unwrap();
        let back = load_from(&path).unwrap();
        assert_eq!(back.active_provider_id.as_deref(), Some("custom-acme"));
        assert!(back.provider("custom-acme").is_some());
        // 文件里绝不能出现任何形似 key 的字段
        let raw = std::fs::read_to_string(&path).unwrap();
        assert!(!raw.contains("apiKey") && !raw.contains("sk-"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
