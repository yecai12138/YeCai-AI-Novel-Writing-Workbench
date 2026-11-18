//! 提示词优化：把用户的口语指令改写成「主 AI 编排」能稳定执行的任务规格。
//!
//! 三条边界：
//! 1. **真的调模型**：用设置里当前激活的提供商 + 它的 API Key（凭据库里有，
//!    命令只读不写，也绝不回传明文），协议按 openai / anthropic 分流；
//! 2. **不编事实**：模型只做结构化与消歧，用户没给的章号、字数、文件名一律不许补，
//!    缺哪项就写进 notes，让人自己补；
//! 3. **兜底在前端**：没 Key、模型报错、超时都返回明确错误，前端回落到本地规则并显示原因，
//!    优化按钮不能因为网络失败就变成死按钮。

use serde::{Deserialize, Serialize};
use tauri::AppHandle;

use crate::models::settings::ProviderConfig;
use crate::services::{secrets, settings};

/// 优化结果。`source` 让界面能如实显示「这是模型改的」还是「本地规则兜的」。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PolishView {
    pub text: String,
    pub provider: String,
    pub model: String,
}

/// 让模型只做整形、不做创作。规则写成硬约束，避免它顺手把「第 30 至 40 章正文」
/// 理解成「大纲」（用户报的正是这种偏差）。
const SYSTEM_PROMPT: &str = r#"你是「野菜·AI小说工作台」的提示词整形器。用户会把一句口语的中文写作指令发给你，你要把它改写成执行规格，供下游编排器拆计划。

输出格式（固定八段，缺一项就按下面规则省略，不许自创段落）：
【本次只做一件事】<唯一交付物：正文 / 章级细纲 / 全书大纲 / 设定 / 拆对标 / 找资料 / 只读检查>
【产出】<要写或要改的具体文件路径，如 正文/第NNN章_<小标题>.md、大纲/细纲_第NNN章.md、大纲/大纲.md>
【范围】<章号区间，如 第 301—330 章（共 30 章）；一步只写一章>
【字数】<用户给的字数，原样保留；没给就不写这一段>
【总字数】<用户给的是整本目标（如「全书40万字」）时写这一段，不当单章下限>
【收尾】<只有用户明确要求把书写完/结尾/收尾时写这一段>
【依据】<用户提到参考资料/对标/大纲/设定/追踪时写：先读哪些文件再动笔>
【不要做】<这一轮明确不该做的事>
【原话】<逐字复制用户原话，不做任何改写>

硬约束：
1. 只整形，不创作。不许补用户没给的章号、字数、角色名、情节、文件名。
2. 「根据大纲」「按设定」「参考追踪」里的 大纲/设定/追踪 是**参考资料**，不是这一轮的交付物；交付物只看用户要写什么（正文/细纲/大纲）。
3. 用户同时要两件互斥的事（例如「重新生成全书大纲」＋「只写第7章细纲」）时，按原话里先出现的那个定型，另起一行写【冲突】说明，并要求另一件单独下发。
4. 字数：写给正文时是「每章纯正文 X 字，写完自查」；写给细纲时是「把 X 写进细纲的『正文目标字数』」。
5. 收尾：只有「结尾处理/写结尾/作为整本书的结尾/大结局/完结/终章/完本/全书收束」这类才算；「结尾钩子」「章末留钩」是写法要求，不算收尾请求。
6. 说法含糊、信息不足时，不要猜——把缺的那一项写成「【缺口】缺什么，补一句什么能让派发不缩水」。
7. 全文用简体中文，不要解释你在做什么，不要加代码块围栏，直接输出规格正文。"#;

/// 优化一条指令。前端拿不到 Key 的明文，只能让它去取。
#[tauri::command]
pub async fn polish_prompt(app: AppHandle, draft: String) -> Result<PolishView, String> {
    let draft = draft.trim().to_string();
    if draft.is_empty() {
        return Err("输入是空的——先写一句要它做什么".to_string());
    }

    let current = settings::load(&app);
    let provider = current
        .active_provider_id
        .as_deref()
        .and_then(|id| current.providers.iter().find(|p| p.id == id))
        .cloned()
        .or_else(|| current.providers.first().cloned())
        .ok_or_else(|| "设置里还没有模型提供商：先在 设置 → 模型提供商 添加一个并填 API Key".to_string())?;

    if provider.protocol == "bedrock" {
        return Err(format!(
            "提供商「{}」走 Bedrock 签名，提示词优化暂不支持；换一个 openai / anthropic 协议的提供商",
            provider.name
        ));
    }

    let base = provider.base_url.trim().trim_end_matches('/').to_string();
    if base.is_empty() {
        return Err(format!(
            "提供商「{}」的 API 地址是空的：先填 baseUrl（例如 https://api.openai.com/v1）",
            provider.name
        ));
    }

    let model = provider
        .model_id()
        .map(|m| m.to_string())
        .ok_or_else(|| format!("提供商「{}」还没有可用模型：先在设置里拉取或手填一个", provider.name))?;

    let key = secrets::get_provider_key(&provider.id)
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .ok_or_else(|| {
            format!(
                "提供商「{}」没有 API Key：在 设置 → 模型提供商 里填上再点优化",
                provider.name
            )
        })?;

    let text = call_model(&provider, &base, &model, &key, &draft).await?;
    let text = strip_fences(&text);
    if text.trim().is_empty() {
        return Err("模型返回了空内容，重试一次或换模型".to_string());
    }

    Ok(PolishView {
        text,
        provider: provider.name.clone(),
        model,
    })
}

/// 按协议发一次非流式补全。超时 45 秒——优化是交互动作，不能挂着不走。
async fn call_model(
    provider: &ProviderConfig,
    base: &str,
    model: &str,
    key: &str,
    draft: &str,
) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .build()
        .map_err(|e| format!("HTTP 客户端创建失败：{e}"))?;

    let (url, request) = if provider.protocol == "anthropic" {
        let url = format!("{base}/v1/messages");
        let body = serde_json::json!({
            "model": model,
            "max_tokens": 1200,
            "temperature": 0.2,
            "system": SYSTEM_PROMPT,
            "messages": [{ "role": "user", "content": draft }],
        });
        let req = client
            .post(&url)
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body);
        (url, req)
    } else {
        let url = format!("{base}/chat/completions");
        let body = serde_json::json!({
            "model": model,
            "temperature": 0.2,
            "max_tokens": 1200,
            "stream": false,
            "messages": [
                { "role": "system", "content": SYSTEM_PROMPT },
                { "role": "user", "content": draft },
            ],
        });
        let req = client
            .post(&url)
            .header("Authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .json(&body);
        (url, req)
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
            "{url} 返回 {}：{}",
            status.as_u16(),
            snippet.trim()
        ));
    }

    extract_text(&body).ok_or_else(|| {
        let snippet: String = body.chars().take(400).collect();
        format!("响应里没有文本内容：{}", snippet.trim())
    })
}

/// OpenAI：`choices[0].message.content`；Anthropic：`content[{type:text}].text`。
fn extract_text(body: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct OpenAiMessage {
        content: Option<String>,
    }
    #[derive(Deserialize)]
    struct OpenAiChoice {
        message: Option<OpenAiMessage>,
    }
    #[derive(Deserialize)]
    struct OpenAiResponse {
        choices: Option<Vec<OpenAiChoice>>,
    }
    #[derive(Deserialize)]
    struct AnthropicBlock {
        #[serde(rename = "type")]
        kind: Option<String>,
        text: Option<String>,
    }
    #[derive(Deserialize)]
    struct AnthropicResponse {
        content: Option<Vec<AnthropicBlock>>,
    }

    if let Ok(v) = serde_json::from_str::<OpenAiResponse>(body) {
        if let Some(text) = v
            .choices
            .and_then(|c| c.into_iter().next())
            .and_then(|c| c.message)
            .and_then(|m| m.content)
        {
            return Some(text);
        }
    }
    if let Ok(v) = serde_json::from_str::<AnthropicResponse>(body) {
        let text: String = v
            .content
            .unwrap_or_default()
            .into_iter()
            .filter(|b| b.kind.as_deref().unwrap_or("text") == "text")
            .filter_map(|b| b.text)
            .collect::<Vec<_>>()
            .join("");
        if !text.trim().is_empty() {
            return Some(text);
        }
    }
    None
}

/// 模型爱把整段包进三反引号，去掉围栏，但内容原样保留。
fn strip_fences(text: &str) -> String {
    let t = text.trim();
    let t = t
        .strip_prefix("\u{60}\u{60}\u{60}text")
        .or_else(|| t.strip_prefix("\u{60}\u{60}\u{60}markdown"))
        .or_else(|| t.strip_prefix("\u{60}\u{60}\u{60}"))
        .unwrap_or(t);
    let t = t.strip_suffix("\u{60}\u{60}\u{60}").unwrap_or(t);
    t.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openai_shape_is_read() {
        let body = r#"{"choices":[{"message":{"content":"【本次只做一件事】正文"}}]}"#;
        assert_eq!(extract_text(body).as_deref(), Some("【本次只做一件事】正文"));
    }

    #[test]
    fn anthropic_shape_concatenates_text_blocks() {
        let body = r#"{"content":[{"type":"text","text":"甲"},{"type":"text","text":"乙"}]}"#;
        assert_eq!(extract_text(body).as_deref(), Some("甲乙"));
    }

    #[test]
    fn fences_are_stripped_but_content_kept() {
        let raw = "\u{60}\u{60}\u{60}text\n【本次只做一件事】细纲\n\u{60}\u{60}\u{60}";
        assert_eq!(strip_fences(raw), "【本次只做一件事】细纲");
    }
}
