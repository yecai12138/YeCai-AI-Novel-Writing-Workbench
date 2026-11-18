//! 会话消息：主 AI 面板里聊过的东西，按书存盘（`.storydesk/chats/<session>.json`）。
//!
//! 设计取舍：
//! 1. **一条消息一个结构体**，`role` 用字符串而不是枚举——前端的聊天流还会加新角色
//!    （比如以后的「工具调用」），枚举一改就要同步改 TS，代价比收益大。
//! 2. `plan` 单独开字段而不是塞进 `content`：重启后要重新渲染「计划确认卡」，
//!    从字符串里再解析一遍 JSON 太脆。
//! 3. `payload` 是给派发卡预留的通用槽位：`agent_id / avatar / task / index / total`
//!    这些字段放进去，前端 `JSON.parse` 就能还原卡片，不必为每种卡片加一组字段。

use serde::{Deserialize, Serialize};

use crate::models::dto::PlanStep;

/// 消息角色。`user` 作者、`master` 主 AI、`system` 系统提示、`agent` 子智能体。
pub const ROLE_USER: &str = "user";
pub const ROLE_MASTER: &str = "master";
pub const ROLE_SYSTEM: &str = "system";
pub const ROLE_AGENT: &str = "agent";

/// 一条会话消息。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    /// 主 AI 给出的计划（只在 `master` 的计划消息里有值）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<Vec<PlanStep>>,
    /// 计划来源：`llm` / `rules` / `confirmed`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_source: Option<String>,
    /// 阶段 11：「我理解成了什么」，计划卡上直接显示，跟着消息一起落盘。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_intent: Option<String>,
    /// 卡片载荷（派发卡 / 收尾卡都用它还原）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,
    /// 毫秒时间戳。
    #[serde(default)]
    pub timestamp: i64,
}

impl ChatMessage {
    /// 白名单校验：不认识的 role 直接拒掉，避免前端拿到怪东西。
    pub fn is_valid_role(role: &str) -> bool {
        matches!(role, ROLE_USER | ROLE_MASTER | ROLE_SYSTEM | ROLE_AGENT)
    }
}
