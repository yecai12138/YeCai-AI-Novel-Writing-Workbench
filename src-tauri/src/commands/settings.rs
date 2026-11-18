//! 设置与第三方模型提供商命令。
//!
//! 三条边界（阶段 5 需求）：
//! 1. **配置里不存 Key**。`settings.json` 只有提供商元数据（id/name/baseUrl/protocol/models），
//!    Key 一律进系统钥匙串（`services::secrets`，account = provider id）；
//! 2. **Key 单向**。前端只能拿到 `hasKey: bool`，`set_provider_key` 返回的也是状态对象，
//!    任何命令都不会把明文 key 回传；
//! 3. `fetch_available_models` 是真实可用的网络调用（`/models`），不是置灰占位。

use tauri::AppHandle;

use crate::models::settings::{AppSettings, ProviderConfig, SettingsView, PROTOCOLS};
use crate::models::StoryError;
use crate::services::{secrets, settings};

/// 读设置（含每个提供商是否有 Key，以及配置文件的真实路径）。
#[tauri::command]
pub async fn get_settings(app: AppHandle) -> Result<SettingsView, StoryError> {
    settings::view(&app)
}

/// 覆盖写设置。`normalize()` 会修掉非法 id、悬空的 activeProviderId 等脏数据。
#[tauri::command]
pub async fn save_settings(
    app: AppHandle,
    settings: AppSettings,
) -> Result<SettingsView, StoryError> {
    let handle = app.clone();
    crate::commands::blocking(move || crate::services::settings::save(&handle, settings)).await
}

/// 写某个提供商的 Key（空串 = 删除）。
#[tauri::command]
pub async fn set_provider_key(
    app: AppHandle,
    provider_id: String,
    key: String,
) -> Result<SettingsView, StoryError> {
    let handle = app.clone();
    crate::commands::blocking(move || {
        let _ = secrets::save_provider_key(&provider_id, &key)?;
        settings::view(&handle)
    })
    .await
}

/// 查某个提供商是否有 Key（不返回明文）。
#[tauri::command]
pub async fn check_provider_key(provider_id: String) -> Result<bool, StoryError> {
    crate::commands::blocking(move || Ok(secrets::has_provider_key(&provider_id))).await
}

/// 删掉某个提供商的 Key。
#[tauri::command]
pub async fn delete_provider_key(
    app: AppHandle,
    provider_id: String,
) -> Result<SettingsView, StoryError> {
    let handle = app.clone();
    crate::commands::blocking(move || {
        let _ = secrets::delete_provider_key(&provider_id)?;
        settings::view(&handle)
    })
    .await
}

/// 删除一个提供商：配置、当前标记、凭据库里的 Key 一起清掉。
///
/// - `app` 是 Tauri 注入的句柄（前端只传 `providerId`），没有它定位不到 `settings.json`；
/// - 安全红线：删的正好是当前激活项 → `activeProviderId` 置 `None`，**不回落到别的提供商**；
/// - 内置提供商要记进 `removedBuiltins`，否则下次启动会被「补齐内置项」逻辑加回来；
/// - 凭据清理失败不算配置删除失败，但要如实把原因回报给界面。
#[tauri::command]
pub async fn delete_provider(app: AppHandle, provider_id: String) -> Result<(), String> {
    let handle = app.clone();
    crate::commands::blocking(move || {
        let id = provider_id.trim().to_string();
        if id.is_empty() {
            return Err(StoryError::invalid("providerId", "提供商 id 为空"));
        }

        let mut current = settings::load(&handle);
        let before = current.providers.len();
        current.providers.retain(|p| p.id != id);
        if current.providers.len() == before {
            return Err(StoryError::invalid(
                "providerId",
                format!("没有 id 为「{id}」的提供商"),
            ));
        }

        if AppSettings::is_builtin(&id) && !current.removed_builtins.iter().any(|x| x == &id) {
            current.removed_builtins.push(id.clone());
        }
        if current.active_provider_id.as_deref() == Some(id.as_str()) {
            current.active_provider_id = None;
        }
        settings::save(&handle, current)?;

        // 配置已经删掉了，Key 清理失败不该让界面以为「没删成」
        if let Err(e) = secrets::delete_provider_key(&id) {
            return Err(StoryError::internal(format!(
                "提供商已删除，但清理凭据库里的 API Key 失败：{e}"
            )));
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())
}

/// 拉取提供商的可用模型列表。
/// - `openai` 协议：`GET {base_url}/models` + `Authorization: Bearer {key}`，解析 `data[].id`
/// - `anthropic` 协议：`GET {base_url}/models` + `x-api-key` + `anthropic-version`，解析 `data[].id`
/// - `bedrock`：列模型要走 AWS SigV4 签名，这里返回常用模型清单（并如实说明原因）
#[tauri::command]
pub async fn fetch_available_models(
    base_url: String,
    protocol: String,
    api_key: Option<String>,
) -> Result<Vec<String>, String> {
    let protocol = protocol.trim().to_ascii_lowercase();
    if !PROTOCOLS.contains(&protocol.as_str()) {
        return Err(format!(
            "不认识的协议「{protocol}」，只支持 {}",
            PROTOCOLS.join(" / ")
        ));
    }

    if protocol == "bedrock" {
        return Ok(bedrock_catalog());
    }

    let base = base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("API 地址为空，先填 baseUrl（例如 https://api.openai.com/v1）".to_string());
    }
    let url = format!("{base}/models");

    let key = api_key
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| "还没填 API 密钥，无法请求模型列表".to_string())?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败：{e}"))?;

    let mut request = client.get(&url).header("Accept", "application/json");
    request = if protocol == "anthropic" {
        request
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
    } else {
        request.header("Authorization", format!("Bearer {key}"))
    };

    let response = request
        .send()
        .await
        .map_err(|e| format!("请求 {url} 失败：{e}"))?;

    let status = response.status();
    let body = response
        .text()
        .await
        .map_err(|e| format!("读取响应失败：{e}"))?;

    if !status.is_success() {
        let snippet: String = body.chars().take(400).collect();
        return Err(format!(
            "{} 返回 {}：{}",
            url,
            status.as_u16(),
            snippet.trim()
        ));
    }

    parse_model_ids(&body).map_err(|e| format!("{url} 的响应解析失败：{e}"))
}

/// 从 `{ "data": [{ "id": "..." }, ...] }`（也兼容 `{ "models": [...] }`）里取模型 id。
fn parse_model_ids(body: &str) -> Result<Vec<String>, String> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|e| e.to_string())?;

    let list = value
        .get("data")
        .and_then(|v| v.as_array())
        .or_else(|| value.get("models").and_then(|v| v.as_array()))
        .or_else(|| value.as_array())
        .ok_or_else(|| "响应里没有 data/models 数组".to_string())?;

    let mut ids: Vec<String> = list
        .iter()
        .filter_map(|item| {
            item.get("id")
                .or_else(|| item.get("name"))
                .or_else(|| item.get("modelId"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();

    ids.sort();
    ids.dedup();
    if ids.is_empty() {
        return Err("列表是空的（可能这个地址不支持 /models）".to_string());
    }
    Ok(ids)
}

/// Bedrock 常用模型（列全量需要 AWS SigV4，不便在客户端做）。
fn bedrock_catalog() -> Vec<String> {
    [
        "anthropic.claude-sonnet-4-20250514-v1:0",
        "anthropic.claude-3-7-sonnet-20250219-v1:0",
        "anthropic.claude-3-5-haiku-20241022-v1:0",
        "anthropic.claude-3-5-sonnet-20241022-v2:0",
        "amazon.nova-pro-v1:0",
        "amazon.nova-lite-v1:0",
        "meta.llama3-70b-instruct-v1:0",
        "mistral.mistral-large-2402-v1:0",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// 提供商表单校验（前端「创建提供商」按钮也会走一遍，避免脏数据落盘）。
/// 提供商表单校验：返回 `None` 表示通过，`Some(原因)` 是给界面显示的中文原因。
#[tauri::command]
pub async fn validate_provider(provider: ProviderConfig) -> Result<Option<String>, StoryError> {
    Ok(provider.validate().err())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_style_list() {
        let body =
            r#"{"object":"list","data":[{"id":"gpt-4o"},{"id":"gpt-4o-mini"},{"id":"gpt-4o"}]}"#;
        assert_eq!(
            parse_model_ids(body).unwrap(),
            vec!["gpt-4o", "gpt-4o-mini"]
        );
    }

    #[test]
    fn parses_anthropic_style_list() {
        let body = r#"{"data":[{"id":"claude-sonnet-4-20250514","type":"model"}]}"#;
        assert_eq!(
            parse_model_ids(body).unwrap(),
            vec!["claude-sonnet-4-20250514"]
        );
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_model_ids("<html>404</html>").is_err());
        assert!(parse_model_ids(r#"{"data":[]}"#).is_err());
    }

    #[test]
    fn bedrock_catalog_is_not_empty() {
        assert!(bedrock_catalog().iter().any(|m| m.contains("claude")));
    }
}
