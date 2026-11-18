//! 全局设置：模型提供商、激活提供商、引擎偏好、自动保存偏好。
//!
//! 设计硬规则：
//! - **API Key 绝不进这个结构体**，它只存在系统凭据库（`services::secrets`），
//!   持久化到 `settings.json` 的只有 `providers` 等非敏感配置；
//! - 字段名序列化成 camelCase，前端 TS 类型逐字段对应；
//! - 未知字段忽略、缺失字段用默认值，方便以后加字段不炸旧文件。

use serde::{Deserialize, Serialize};

use crate::models::ai::{normalized_agents, AgentConfig};

/// 一个模型提供商（OpenAI / Anthropic / Moonshot / Bedrock / 自定义网关）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    /// 唯一标识，如 "openai" / "custom-acme"（小写字母、数字、短横线）
    pub id: String,
    /// 显示名称
    pub name: String,
    /// API 地址，如 `https://api.openai.com/v1`
    #[serde(default)]
    pub base_url: String,
    /// 协议：openai | anthropic | bedrock
    #[serde(default = "default_protocol")]
    pub protocol: String,
    /// 用户添加/拉取到的模型 ID
    #[serde(default)]
    pub models: Vec<String>,
    /// 目录里被选中「使用中」的那个模型；没选时引擎回退到 `models` 的第一个。
    /// 实测事故：提供商拉到了 6 个模型却永远只用第一个，用户在目录界面无从选择。
    #[serde(default)]
    pub active_model: Option<String>,
}

fn default_protocol() -> String {
    "openai".to_string()
}

/// 支持的协议。
pub const PROTOCOLS: [&str; 3] = ["openai", "anthropic", "bedrock"];

impl ProviderConfig {
    /// 该协议在 spawn 子进程时注入的 `(环境变量名, 取值)` 主键名。
    pub fn api_key_env(&self) -> &'static str {
        match self.protocol.as_str() {
            "anthropic" => "ANTHROPIC_API_KEY",
            "bedrock" => "AWS_BEARER_TOKEN_BEDROCK",
            _ => "OPENAI_API_KEY",
        }
    }

    /// 该协议的 base URL 环境变量名；Bedrock 没有（走 AWS 自己的配置链）。
    pub fn base_url_env(&self) -> Option<&'static str> {
        match self.protocol.as_str() {
            "anthropic" => Some("ANTHROPIC_BASE_URL"),
            "openai" => Some("OPENAI_BASE_URL"),
            _ => None,
        }
    }

    /// 该提供商是否依赖系统环境变量（Bedrock 常靠 AWS 凭据链，UI 要提示去设环境变量）。
    pub fn supports_env(&self) -> bool {
        self.protocol == "bedrock"
    }

    /// 本次真正要用的模型：用户在目录里选的那个优先；没选（或选的已不在目录）
    /// 回退到目录第一个——与旧行为兼容，不会突然变成「没有模型」。
    pub fn model_id(&self) -> Option<&str> {
        self.active_model
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty() && self.models.iter().any(|x| x == m))
            .or_else(|| self.models.first().map(String::as_str))
    }

    /// 名字里带空格/大写的 id 一律不接受：它要当凭据库的 account 名用。
    pub fn is_valid_id(id: &str) -> bool {
        !id.is_empty()
            && id.len() <= 48
            && id
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
    }

    /// 表单校验：id 合法、名字非空、协议在允许集合里。
    pub fn validate(&self) -> Result<(), String> {
        if !Self::is_valid_id(&self.id) {
            return Err(
                "Provider ID 只能用小写字母、数字、短横线或下划线（例如 custom-acme）".to_string(),
            );
        }
        if self.name.trim().is_empty() {
            return Err("显示名称不能为空".to_string());
        }
        if !PROTOCOLS.contains(&self.protocol.as_str()) {
            return Err(format!(
                "不支持的协议：{}（可选 openai / anthropic / bedrock）",
                self.protocol
            ));
        }
        if self.protocol != "bedrock" && self.base_url.trim().is_empty() {
            return Err("API 地址不能为空（例如 https://api.openai.com/v1）".to_string());
        }
        Ok(())
    }
}

/// 引擎偏好：`"claude"` 或 `"opencode"`（旧的 `"mock"` 会在 normalize 里被改写成 `"claude"`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnginePref {
    /// claude | opencode
    #[serde(default = "default_engine")]
    pub kind: String,
}

fn default_engine() -> String {
    "claude".to_string()
}

/// 引擎偶发错误的退避重试。默认 3 次、1.5s 起、×2 递增、封顶 20s：
/// 1.5s → 3s，够躲过 opencode 本地 server 的重启窗口，又不会让一步卡太久。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RetryPref {
    /// 最多试几次（含首次；1 = 不重试）
    #[serde(default = "default_retry_attempts")]
    pub max_attempts: u32,
    /// 首次重试前等多久（毫秒）
    #[serde(default = "default_retry_base_ms")]
    pub base_ms: u64,
    /// 每次重试的等待倍数
    #[serde(default = "default_retry_factor")]
    pub factor: f32,
    /// 单次等待上限（毫秒）
    #[serde(default = "default_retry_max_ms")]
    pub max_ms: u64,
}

fn default_retry_attempts() -> u32 {
    3
}

fn default_retry_base_ms() -> u64 {
    1500
}

fn default_retry_factor() -> f32 {
    2.0
}

fn default_retry_max_ms() -> u64 {
    20_000
}

impl Default for RetryPref {
    fn default() -> Self {
        Self {
            max_attempts: default_retry_attempts(),
            base_ms: default_retry_base_ms(),
            factor: default_retry_factor(),
            max_ms: default_retry_max_ms(),
        }
    }
}

impl RetryPref {
    /// 第 `attempt` 次重试（从 1 开始）前该等多久。夹在 [0, max_ms] 内，配置写歪也不会卡死。
    pub fn wait_ms(&self, attempt: u32) -> u64 {
        if attempt == 0 || self.base_ms == 0 {
            return 0;
        }
        let factor = if self.factor.is_finite() && self.factor > 0.0 {
            self.factor
        } else {
            1.0
        };
        let raw = self.base_ms as f32 * factor.powi(attempt.saturating_sub(1) as i32);
        let capped = if raw.is_finite() { raw as u64 } else { self.max_ms };
        capped.min(self.max_ms)
    }

    /// 有效尝试次数：至少 1 次，最多 10 次（再多就是死等，不如把错误交给用户看）。
    pub fn attempts(&self) -> u32 {
        self.max_attempts.clamp(1, 10)
    }
}

impl EnginePref {
    /// 合法值清单（normalize 与设置页校验共用）。
    pub fn is_valid_kind(kind: &str) -> bool {
        kind == "claude" || kind == "opencode"
    }
}

impl Default for EnginePref {
    fn default() -> Self {
        Self {
            kind: default_engine(),
        }
    }
}

/// 自动保存偏好。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutosavePref {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_delay")]
    pub delay_ms: u32,
}

fn default_true() -> bool {
    true
}

fn default_delay() -> u32 {
    1000
}

impl Default for AutosavePref {
    fn default() -> Self {
        Self {
            enabled: true,
            delay_ms: default_delay(),
        }
    }
}

/// 全局设置（持久化到 app config dir 的 `settings.json`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppSettings {
    #[serde(default = "default_version")]
    pub version: u32,
    /// 当前激活的提供商 id（None = 还没选过，运行时不注入任何环境变量）
    #[serde(default)]
    pub active_provider_id: Option<String>,
    /// 所有提供商配置（不含 Key）
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// 用户主动删掉的内置提供商 id：不写这个，删掉的内置项会在下次启动被「补齐」回来
    #[serde(default)]
    pub removed_builtins: Vec<String>,
    #[serde(default)]
    pub engine: EnginePref,
    #[serde(default)]
    pub autosave: AutosavePref,
    /// 可定制的子智能体（主 AI 按 id 派活）。空列表会在 normalize 时补默认六角色。
    #[serde(default)]
    pub agents: Vec<AgentConfig>,
    /// 允许子智能体调用的工具白名单（阶段 9）。`None` = 从没配过，按 Read / Write 处理。
    #[serde(default)]
    pub allowed_tools: Option<Vec<String>>,
    /// 引擎偶发错误的退避重试（断点续跑同批上线）
    #[serde(default)]
    pub retry: RetryPref,
    /// 指定的 opencode 可执行文件路径（第 8 项）。非空且文件存在时优于自动定位，
    /// 用来指向桌面版自带的 v2，或切回本地保留的 v1.19.5。
    #[serde(default)]
    pub opencode_path: Option<String>,
}

fn default_version() -> u32 {
    2
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            version: default_version(),
            active_provider_id: Some("anthropic".to_string()),
            providers: builtin_providers(),
            removed_builtins: Vec::new(),
            engine: EnginePref::default(),
            autosave: AutosavePref::default(),
            agents: crate::models::ai::default_agents(),
            allowed_tools: Some(crate::models::ai::default_tools()),
            retry: RetryPref::default(),
            opencode_path: None,
        }
    }
}

impl AppSettings {
    /// 找激活的提供商（找不到返回 None，调用方自行回落）。
    pub fn active_provider(&self) -> Option<&ProviderConfig> {
        let id = self.active_provider_id.as_deref()?;
        self.providers.iter().find(|p| p.id == id)
    }

    /// 按 id 找。
    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.providers.iter().find(|p| p.id == id)
    }

    /// 兜底清洗：去重 id、丢掉非法项、修掉悬空的 activeProviderId、清掉不认识的内置删除记录。
    ///
    /// 注意：`active_provider_id` 指向不存在的提供商时**置 None**，不再回落到第一个。
    /// 删除当前提供商后如果回落，用户会看到「我明明删了它，怎么又成了当前」。
    pub fn normalize(&mut self) {
        let mut seen: Vec<String> = Vec::new();
        self.providers.retain(|p| {
            if !ProviderConfig::is_valid_id(&p.id) || seen.iter().any(|s| s == &p.id) {
                return false;
            }
            seen.push(p.id.clone());
            true
        });
        if let Some(id) = self.active_provider_id.clone() {
            if !self.providers.iter().any(|p| p.id == id) {
                self.active_provider_id = None;
            }
        }
        // 只保留确实存在的内置 id，并且去重
        let builtin: Vec<&str> = BUILTIN_IDS.iter().copied().collect();
        let mut kept: Vec<String> = Vec::new();
        self.removed_builtins.retain(|id| {
            if !builtin.contains(&id.as_str()) || kept.iter().any(|k| k == id) {
                return false;
            }
            kept.push(id.clone());
            true
        });
        if self.autosave.delay_ms < 200 {
            self.autosave.delay_ms = 200;
        }
        // 引擎只认 claude / opencode：老配置里是 "mock" 或别的值，直接改写成 claude，
        // 免得界面上出现一个不存在的选项。
        if !EnginePref::is_valid_kind(&self.engine.kind) {
            self.engine.kind = "claude".to_string();
        }
        // 智能体：去非法 id / 去重 / 修目录；清洗后为空就补默认六角色
        let agents = std::mem::take(&mut self.agents);
        self.agents = normalized_agents(agents);
        // 工具白名单：没配过补默认；不认识的工具名丢掉；空清单是合法值（= 一个都不许调）
        let tools = self
            .allowed_tools
            .take()
            .unwrap_or_else(crate::models::ai::default_tools);
        self.allowed_tools = Some(crate::models::ai::normalize_tools(&tools));
    }

    /// 这个 id 是不是内置提供商（删除时要记进 `removed_builtins`，否则会被补齐回来）。
    pub fn is_builtin(id: &str) -> bool {
        BUILTIN_IDS.contains(&id)
    }
}

/// 内置提供商 id 清单（`is_builtin` 与「补种」逻辑共用，避免每次构造完整配置）。
pub const BUILTIN_IDS: [&str; 4] = ["openai", "anthropic", "moonshot", "bedrock"];

/// 内置的四个提供商（首次启动或文件损坏时用）。
pub fn builtin_providers() -> Vec<ProviderConfig> {
    vec![
        ProviderConfig {
            id: "openai".to_string(),
            name: "OpenAI".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            protocol: "openai".to_string(),
            models: vec![
                "gpt-4.1".to_string(),
                "gpt-4.1-mini".to_string(),
                "gpt-4o".to_string(),
                "o3".to_string(),
            ],
            active_model: None,
        },
        ProviderConfig {
            id: "anthropic".to_string(),
            name: "Anthropic".to_string(),
            base_url: "https://api.anthropic.com".to_string(),
            protocol: "anthropic".to_string(),
            models: vec![
                "claude-sonnet-4-5".to_string(),
                "claude-opus-4-1".to_string(),
                "claude-3-5-sonnet".to_string(),
                "claude-3-5-haiku".to_string(),
            ],
            active_model: None,
        },
        ProviderConfig {
            id: "moonshot".to_string(),
            name: "Moonshot".to_string(),
            base_url: "https://api.moonshot.cn/v1".to_string(),
            protocol: "openai".to_string(),
            models: vec![
                "kimi-k2-0905-preview".to_string(),
                "moonshot-v1-8k".to_string(),
                "moonshot-v1-32k".to_string(),
            ],
            active_model: None,
        },
        ProviderConfig {
            id: "bedrock".to_string(),
            name: "Amazon Bedrock".to_string(),
            base_url: "".to_string(),
            protocol: "bedrock".to_string(),
            models: vec![
                "anthropic.claude-3-5-sonnet-20241022-v2:0".to_string(),
                "amazon.nova-pro-v1:0".to_string(),
            ],
            active_model: None,
        },
    ]
}

/// 前端用的视图：设置本体 + 每个提供商「有没有 Key」（永远不回传 Key 明文）+ 文件位置。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsView {
    pub settings: AppSettings,
    /// 与 providers 等长的 (providerId, hasKey) 列表
    pub provider_keys: Vec<ProviderKeyState>,
    /// settings.json 的绝对路径（设置页展示，便于用户核对「文件里没有明文」）
    pub path: String,
    /// 凭据库后端名
    pub secret_backend: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderKeyState {
    pub provider_id: String,
    pub has_key: bool,
    /// 这个提供商是否支持「用环境变量替代填写 Key」（目前只有 bedrock）。
    pub supports_env: bool,
    /// 对应要读的环境变量名，前端拿它拼文案「…或设置环境变量 XXX」
    pub key_env: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(models: &[&str], active: Option<&str>) -> ProviderConfig {
        ProviderConfig {
            id: "p".to_string(),
            name: "P".to_string(),
            base_url: String::new(),
            protocol: "openai".to_string(),
            models: models.iter().map(|s| s.to_string()).collect(),
            active_model: active.map(str::to_string),
        }
    }

    /// 选中的模型优先；没选回退第一个；选了但已被删掉也回退第一个。
    #[test]
    fn model_id_prefers_active_and_falls_back() {
        assert_eq!(provider(&["a", "b"], None).model_id(), Some("a"));
        assert_eq!(provider(&["a", "b"], Some("b")).model_id(), Some("b"));
        assert_eq!(provider(&["a", "b"], Some("已删")).model_id(), Some("a"));
        assert_eq!(provider(&["a", "b"], Some("  ")).model_id(), Some("a"));
        assert_eq!(provider(&[], Some("x")).model_id(), None);
        assert_eq!(provider(&[], None).model_id(), None);
    }

    /// 旧 settings.json 没有 activeModel 字段也必须能反序列化（serde default）。
    #[test]
    fn old_settings_json_without_active_model_still_parses() {
        let json =
            r#"{"id":"deep","name":"Deep","baseUrl":"","protocol":"openai","models":["m1"]}"#;
        let p: ProviderConfig = serde_json::from_str(json).expect("旧格式应可解析");
        assert_eq!(p.active_model, None);
        assert_eq!(p.model_id(), Some("m1"));
    }
}
