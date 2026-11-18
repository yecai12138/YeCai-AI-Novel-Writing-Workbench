//! Hook 阻断文案解析。
//!
//! oh-story 的 `guard-outline-before-prose.sh` 命中时 **exit 2 + stderr**，文案是固定的几类。
//! 这里把 stderr 归一成结构化原因，前端据此渲染「缺少前置文件 → 一键生成细纲」这类可操作卡片。
//!
//! 文案来源（v0.10.0，逐字）：
//! ```text
//! ⛔ 写正文被拦截：第 1 章缺少细纲（大纲/细纲_第001章.md）。
//! ⛔ 写正文被拦截：第 2 章的细纲（大纲/细纲_第002章.md）是空的（不计 # 号和空白不到 300 字）。
//! ⛔ 写正文被拦截：上一章（正文/第001章_x.md）有未清毒句式欠账，先清零再写第 2 章；…
//! ```
//!
//! 事件层三条是 storydesk 自己产的（`gate::event_precheck`），不走 hook：
//! ```text
//! ⛔ 写正文被拦截：第 12 章声称属于事件 003，但 大纲/事件/ 下没有这张卡。…
//! ⛔ 写正文被拦截：第 12 章声称属于事件 003（码头交接），但该事件已结算，不能再往里加章。…
//! ⛔ 写正文被拦截：第 12 章声称属于事件 003（码头交接），但事件卡的「章型分工」里没有这一章。…
//! ```

use regex::Regex;
use std::sync::OnceLock;

use crate::models::dto::{GateOutput, HookBlock, HookBlockKind};

fn re_missing() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"缺少细纲（大纲/细纲_第(\d+)章\.md）").expect("missing-outline 正则编译失败")
    })
}

fn re_too_short() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"的细纲（(.+?)）是空的").expect("outline-too-short 正则编译失败"))
}

fn re_event() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"第 (\d+) 章声称属于事件 (\d+)").expect("event-block 正则编译失败")
    })
}

fn re_toxic() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"有未清毒句式欠账").expect("toxic-debt 正则编译失败"))
}

/// stderr → (阻断类型, 一句话原因)。
///
/// 顺序有讲究：先匹配最具体的「缺少细纲」，再匹配「细纲是空的」，
/// 最后才是宽泛的「毒句式欠账」。全不中回退 `Unknown`（并保留原文给用户看）。
pub fn classify(stderr: &str) -> (HookBlockKind, String) {
    let text = stderr.replace("\r\n", "\n");

    if let Some(caps) = re_missing().captures(&text) {
        let chapter: u32 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(1);
        return (
            HookBlockKind::MissingOutline { chapter },
            format!("第 {chapter} 章缺少细纲（大纲/细纲_第{chapter:03}章.md）"),
        );
    }

    if let Some(caps) = re_too_short().captures(&text) {
        let outline = caps
            .get(1)
            .map(|m| m.as_str().trim().to_string())
            .unwrap_or_default();
        return (
            HookBlockKind::OutlineTooShort {
                outline: outline.clone(),
            },
            format!("细纲内容过短：{outline}"),
        );
    }

    if let Some(caps) = re_event().captures(&text) {
        let chapter: u32 = caps
            .get(1)
            .and_then(|m| m.as_str().parse().ok())
            .unwrap_or(1);
        let event = caps
            .get(2)
            .map(|m| m.as_str().to_string())
            .unwrap_or_default();
        if text.contains("没有这张卡") {
            return (
                HookBlockKind::EventMissing { chapter, event: event.clone() },
                format!("第 {chapter} 章声称属于事件 {event}，但 大纲/事件/ 下没有这张卡"),
            );
        }
        if text.contains("已结算") {
            return (
                HookBlockKind::EventSettled { chapter, event: event.clone() },
                format!("第 {chapter} 章声称属于事件 {event}，该事件已结算"),
            );
        }
        if text.contains("里没有这一章") {
            return (
                HookBlockKind::EventUnplanned { chapter, event: event.clone() },
                format!("第 {chapter} 章声称属于事件 {event}，但事件卡的章型分工里没有这一章"),
            );
        }
    }

    if re_toxic().is_match(&text) {
        return (
            HookBlockKind::ToxicDebtPrev,
            "上一章有未清毒的 AI 句式欠账，先清零才能写下一章".to_string(),
        );
    }

    (HookBlockKind::Unknown, first_line(&text))
}

/// 兜底：未知阻断也给出可读原因。
fn first_line(text: &str) -> String {
    let line = text
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.trim_start_matches('⛔').trim();
    if line.is_empty() {
        "写正文被拦截（hook 未给出原因）".to_string()
    } else {
        line.to_string()
    }
}

/// 组装成前端卡片载荷：类型 + 原因 + 建议动作。
pub fn to_block(run_id: &str, stderr: &str) -> HookBlock {
    let (block, reason) = classify(stderr);
    block_from(run_id, block, reason, stderr.to_string())
}

/// 门禁脚本（`storyctl.py`）的拦截文案和 hook 的 stderr 不是一套格式：
/// `--plain` 是白话（`这次没查成：第 1 章没有细纲，或者细纲不止一份。`），
/// 不带 `--plain` 是 JSON（`{"error_code":"CHECK_FAILED","message":"chapter 1 must have
/// exactly one outline file"}`）。两条路都先归一到这里，再落成同一张阻断卡片。
pub fn gate_block(run_id: &str, gate: &GateOutput, chapter: u32) -> HookBlock {
    let mut raw = gate.stdout.trim().to_string();
    if !gate.stderr.trim().is_empty() {
        if !raw.is_empty() {
            raw.push('\n');
        }
        raw.push_str(gate.stderr.trim());
    }

    let (block, reason) = match classify_gate(&gate.stdout, chapter) {
        Some(hit) => hit,
        // stdout 不是可识别的体检结论：退回解析 stderr（可能是 hook 的原文）。
        None => classify(&gate.stderr),
    };

    block_from(run_id, block, reason, raw)
}

fn classify_gate(output: &str, chapter: u32) -> Option<(HookBlockKind, String)> {
    let text = output.trim();
    if text.is_empty() {
        return None;
    }

    if let Ok(value) = serde_json::from_str::<serde_json::Value>(text) {
        let code = value
            .get("error_code")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let message = value.get("message").and_then(|v| v.as_str()).unwrap_or("");
        let lower = message.to_ascii_lowercase();
        if lower.contains("outline file") || message.contains("细纲") {
            return Some((
                HookBlockKind::MissingOutline { chapter },
                format!("第 {chapter} 章缺少细纲（storyctl：{message}）"),
            ));
        }
        if code == "CHECK_FAILED" {
            return Some((
                HookBlockKind::Unknown,
                format!("storyctl 章节体检未通过：{message}"),
            ));
        }
        return None;
    }

    if text.contains("没有细纲") || text.contains("细纲不止一份") || text.contains("细纲是空的")
    {
        return Some((
            HookBlockKind::MissingOutline { chapter },
            format!("第 {chapter} 章缺少细纲（storyctl 原话：{text}）"),
        ));
    }
    if text.contains("未清毒") || text.contains("欠账") {
        return Some((
            HookBlockKind::ToxicDebtPrev,
            format!("上一章有未清毒句式欠账（storyctl 原话：{text}）"),
        ));
    }
    Some((
        HookBlockKind::Unknown,
        format!("storyctl 章节体检未通过：{text}"),
    ))
}

fn block_from(run_id: &str, block: HookBlockKind, reason: String, raw: String) -> HookBlock {
    let (action, chapter) = match &block {
        HookBlockKind::MissingOutline { chapter } => {
            ("generate_outline".to_string(), Some(*chapter))
        }
        HookBlockKind::OutlineTooShort { .. } => ("generate_outline".to_string(), None),
        HookBlockKind::ToxicDebtPrev => ("open_prev_chapter".to_string(), None),
        HookBlockKind::EventMissing { chapter, .. } => ("generate_event".to_string(), Some(*chapter)),
        HookBlockKind::EventSettled { .. } => ("open_event".to_string(), None),
        HookBlockKind::EventUnplanned { chapter, .. } => ("update_event".to_string(), Some(*chapter)),
        HookBlockKind::Unknown => ("none".to_string(), None),
    };

    HookBlock {
        run_id: run_id.to_string(),
        block,
        reason,
        stderr: raw,
        action,
        chapter,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_outline() {
        let raw = "⛔ 写正文被拦截：第 1 章缺少细纲（大纲/细纲_第001章.md）。";
        let (kind, reason) = classify(raw);
        match kind {
            HookBlockKind::MissingOutline { chapter } => assert_eq!(chapter, 1),
            other => panic!("应识别为 MissingOutline，实际 {other:?}"),
        }
        assert!(reason.contains("第 1 章"));
    }

    #[test]
    fn outline_too_short() {
        let raw = "⛔ 写正文被拦截：第 2 章的细纲（大纲/细纲_第002章.md）是空的（不计 # 号和空白不到 300 字）。";
        let (kind, _) = classify(raw);
        match kind {
            HookBlockKind::OutlineTooShort { outline } => {
                assert_eq!(outline, "大纲/细纲_第002章.md")
            }
            other => panic!("应识别为 OutlineTooShort，实际 {other:?}"),
        }
    }

    #[test]
    fn toxic_debt() {
        let raw =
            "⛔ 写正文被拦截：上一章（正文/第001章_开篇.md）有未清毒句式欠账，先清零再写第 2 章。";
        let (kind, _) = classify(raw);
        assert!(matches!(kind, HookBlockKind::ToxicDebtPrev));
    }

    #[test]
    fn unknown_fallback_keeps_reason() {
        let (kind, reason) = classify("⛔ 写正文被拦截：短篇缺少小节大纲（短篇/x/小节大纲.md）。");
        assert!(matches!(kind, HookBlockKind::Unknown));
        assert!(reason.contains("短篇缺少小节大纲"));
    }
}
