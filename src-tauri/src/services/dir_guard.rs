//! 子智能体的**目录硬隔离**：真正拦在 hook 层，而不是只在 prompt 里写句狠话。
//!
//! 三件事：
//! 1. 往 `<书>/.claude/hooks/` 生成 `guard-agent-dir.sh` + `guard-agent-dir.py`（可重复生成，内容固定）。
//! 2. 把 hook 合并进 `<书>/.claude/settings.json` 的 `PreToolUse`(Write|Edit|MultiEdit)，
//!    已经登记过就不重复追加（幂等）。
//! 3. 提供 [`check_writes`]：本轮跑完用「磁盘前后差分」再核一次，谁越界直接点名。
//!
//! 为什么两边都做：hook 只在宿主 CLI 真的读 `.claude/settings.json` 时生效（本项目用 shim
//! 验引擎时它根本不跑），而 `check_writes` 不依赖任何 CLI，任何时候都能给证据。

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::models::StoryError;
use crate::services::atomic_write;

/// hook 脚本相对书根的路径。
pub const HOOK_REL: &str = ".claude/hooks/guard-agent-dir.sh";
/// hook 对应的 python 实现（bash 只做转发，解析 JSON 交给 python，避免手写 sed）。
pub const HOOK_PY_REL: &str = ".claude/hooks/guard-agent-dir.py";
/// 子进程里读的「本次允许目录」环境变量。
pub const ALLOW_ENV: &str = "STORYDESK_AGENT_DIR";
/// bash 转发层找不到 python 时留下的标记（相对书根）。
/// hook 本身 fail-open（不能把作者卡死），但不能让界面以为「已硬隔离」。
pub const NEEDS_PYTHON_REL: &str = ".claude/hooks/.guard-needs-python";

const SH: &str = r#"#!/usr/bin/env bash
# 由「野菜 · AI 小说工作台」自动生成：子智能体只能改自己目录下的文件。别手改，会被覆盖。
set -u
here="$(cd "$(dirname "$0")" && pwd)"
python_bin="${PYTHON:-python3}"
marker="$here/.guard-needs-python"
if ! command -v "$python_bin" >/dev/null 2>&1; then
  # 没 python 只能放行（别把作者卡死），但要留下痕迹：Rust 侧看到这个文件
  # 就知道「目录硬隔离其实没生效」，会在引擎日志里告警，而不是继续假装已隔离。
  : > "$marker" 2>/dev/null
  exit 0
fi
rm -f "$marker" 2>/dev/null
exec "$python_bin" "$here/guard-agent-dir.py"
"#;

const PY: &str = r#"#!/usr/bin/env python3
"""子智能体目录硬隔离：只允许写 STORYDESK_AGENT_DIR 下面的文件。

读 stdin 的 hook JSON（Claude Code 的 PreToolUse 载荷），命中越权就 exit 2 + stderr，
宿主会把这个 stderr 原样显示给模型，写入不会发生。
"""
import json
import os
import re
import sys


def pick_target(data):
    for key in ("file_path", "path", "filePath", "filepath"):
        value = data.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    tool_input = data.get("tool_input")
    if isinstance(tool_input, dict):
        for key in ("file_path", "path", "filePath", "filepath"):
            value = tool_input.get(key)
            if isinstance(value, str) and value.strip():
                return value.strip()
    return ""


# 与 Rust 侧 services::dir_guard::memory_write_exempt 同一条规则：
# `记忆/` 下允许新增文件，但 `记忆/摘要索引.md` 归编排器独占。
# 以前 Rust 放行、这一层拦下，同一个写手写记忆文件会拿到 exit 2（越权），
# 两套规则互相矛盾，用户看到的是「明明允许却写不进去」。
SUMMARY_INDEX_REL = "记忆/摘要索引.md"


def is_memory_exempt(real, allowed):
    """`real` 在 <书>/记忆/ 下且不是摘要索引 → 放行。"""
    book_root = os.path.dirname(allowed)
    memory = os.path.realpath(os.path.join(book_root, "记忆"))
    if real != memory and not real.startswith(memory + os.sep):
        return False
    rel = os.path.relpath(real, book_root).replace(os.sep, "/")
    return rel != SUMMARY_INDEX_REL


def deny(text):
    """绕开控制台编码把拦截文案写给模型：中文直接 print 会抛 UnicodeEncodeError，
    退出码从 2 变成 1，宿主就以为「脚本崩了」而不是「被拦下」。"""
    try:
        sys.stderr.buffer.write(text.encode("utf-8"))
        sys.stderr.buffer.flush()
    except Exception:
        pass


def bash_command(data):
    """取出 Bash/PowerShell 工具调用的命令行（Claude Code 是 tool_input.command）。"""
    tool_input = data.get("tool_input")
    if isinstance(tool_input, dict):
        value = tool_input.get("command")
        if isinstance(value, str) and value.strip():
            return value
    value = data.get("command")
    if isinstance(value, str) and value.strip():
        return value
    return ""


# 为什么拦这条：第 037 章事故。写手用
#   $t = Get-Content -Raw -LiteralPath "正文\第037章.md"; ... ; Set-Content -LiteralPath $p -Value $t -Encoding UTF8
# 改正文。Windows PowerShell 5.1 的 Get-Content 对**无 BOM 的 UTF-8** 文件按系统 ANSI
# （简体中文机器上是 cp936）解码，中文全变成「绗?37绔?琛ヤ綅浜」这种乱码，Set-Content -Encoding UTF8
# 再把它写回去并加 BOM：文件从此变成合法 UTF-8 的乱码，正文内容不可逆损坏（GBK 解码时丢的字节
# 变成 '?'，反解不回来）。所以要改稿只能用 Write/Edit 工具，或者用 python 显式 encoding='utf-8'。
WRITE_VERBS = (
    "Set-Content",
    "Out-File",
    "Add-Content",
    "Clear-Content",
    "Set-Item",
    "Rename-Item",
    "Move-Item",
    "Copy-Item",
    "[IO.File]::WriteAll",
    "[System.IO.File]::WriteAll",
    "[IO.File]::AppendAll",
    "[System.IO.File]::AppendAll",
)
CONTENT_DIRS = ("正文", "大纲", "设定", "追踪", "记忆", "参考资料", "对标")
ENCODING_MESSAGE = (
    "⛔ 别用 PowerShell 的 Get-Content / Set-Content 改书里的文件：Windows PowerShell 5.1 "
    "会把无 BOM 的 UTF-8 按 ANSI(cp936) 读成乱码，写回去正文就废了（第037章事故）。\n"
    "要改稿请用 Write / Edit 工具；必须写脚本时用 python，读写都显式 encoding='utf-8'，"
    "并且不要加 BOM。\n"
)


def forbidden_bash(command):
    """这条命令是不是「用 PowerShell 改写书内文件」？读命令一律放行。"""
    if not command:
        return False
    if not any(d in command for d in CONTENT_DIRS) and ".md" not in command:
        return False
    for verb in WRITE_VERBS:
        if verb in command:
            return True
    # `> 文件` 这种重定向也算写盘（`2>&1` / `-gt` 不算）
    if re.search(r"(?<![\w-])>(?!>|&)(?!\s*-)", command):
        return True
    return False


def main():
    # 一定要按字节读再显式用 UTF-8 解码：Windows 控制台默认是 cp936，
    # sys.stdin.read() 遇到中文路径会解码失败 → json 解析失败 → 静默放行。
    raw_bytes = sys.stdin.buffer.read()
    try:
        raw = raw_bytes.decode("utf-8")
    except UnicodeDecodeError:
        raw = raw_bytes.decode(sys.getfilesystemencoding() or "utf-8", "replace")
    try:
        data = json.loads(raw) if raw.strip() else {}
    except Exception:
        return 0
    if not isinstance(data, dict):
        return 0

    # 编码守卫：先用 PowerShell 写盘命令改书里的文件 → 直接拦下（见 forbidden_bash 的注释）。
    command = bash_command(data)
    if command and forbidden_bash(command):
        deny(ENCODING_MESSAGE)
        return 2

    allowed = os.environ.get("STORYDESK_AGENT_DIR", "").strip()
    if not allowed:
        return 0
    allowed = os.path.realpath(allowed)
    target = pick_target(data)
    if not target:
        return 0
    real = os.path.realpath(target)
    if real == allowed or real.startswith(allowed + os.sep):
        return 0
    if is_memory_exempt(real, allowed):
        return 0
    try:
        shown = os.path.relpath(real, os.path.dirname(allowed))
    except Exception:
        shown = real
    message = (
        "\u26d4 \u76ee\u5f55\u8d8a\u6743\uff1a%s \u4e0d\u5728 %s/ \u4e0b\uff0c\u5df2\u62e6\u622a\u3002\n"
        % (shown, os.path.basename(allowed))
    )
    deny(message)
    return 2


if __name__ == "__main__":
    sys.exit(main())
"#;

/// 生成 hook 脚本并登记到 `.claude/settings.json`；返回脚本路径（失败不致命）。
pub fn ensure(book_root: &Path) -> Result<PathBuf, StoryError> {
    let sh = book_root.join(HOOK_REL);
    let py = book_root.join(HOOK_PY_REL);
    atomic_write::write_atomic(&sh, SH.as_bytes())?;
    atomic_write::write_atomic(&py, PY.as_bytes())?;
    register_in_settings(book_root)?;
    Ok(sh)
}

/// 把 hook 合并进 `.claude/settings.json`（幂等）。
fn register_in_settings(book_root: &Path) -> Result<(), StoryError> {
    let path = book_root.join(".claude").join("settings.json");
    // 读失败 / 解析失败**不能**当成空对象：下面会用「只有一个 PreToolUse」的对象整份覆盖，
    // 用户手改的 permissions、别的 hooks 就静默消失了（BOM、尾逗号、JSON 里的注释都能触发；
    // 记事本存成「UTF-8 with BOM」是最常见的那一种）。这里宁可返回错误、一个字节都不写。
    // 读法统一走 atomic_write::read_text：它会剥掉 UTF-8 BOM，与其它读文本的地方口径一致。
    let mut root: Value = if path.exists() {
        let text = atomic_write::read_text(&path).map_err(|e| {
            StoryError::invalid(
                "settings.json",
                format!(
                    "{} 读不出来，已放弃写入（不会覆盖你的配置）：{e}",
                    path.display()
                ),
            )
        })?;
        serde_json::from_str(&text).map_err(|e| {
            StoryError::invalid(
                "settings.json",
                format!(
                    "{} 不是合法 JSON，已放弃写入以免覆盖你的配置；请修好或删掉它后重试：{e}",
                    path.display()
                ),
            )
        })?
    } else {
        json!({})
    };
    if !root.is_object() {
        return Err(StoryError::invalid(
            "settings.json",
            format!(
                "{} 的顶层不是对象，已放弃写入以免覆盖你的配置",
                path.display()
            ),
        ));
    }

    // matcher 里必须带 Bash：第 037 章的正文是被 PowerShell 的
    // `Get-Content … | Set-Content -Encoding UTF8` 写坏的，而 Bash 不在旧 matcher 里，
    // hook 根本没被调用，编码守卫也就没机会拦。
    let entry = json!({
        "matcher": "Write|Edit|MultiEdit|Bash",
        "hooks": [{ "type": "command", "command": "bash .claude/hooks/guard-agent-dir.sh" }]
    });

    let hooks = root
        .as_object_mut()
        .expect("刚保证过是对象")
        .entry("hooks")
        .or_insert_with(|| json!({}));
    if !hooks.is_object() {
        return Err(StoryError::invalid(
            "settings.json",
            format!(
                "{} 里的 hooks 不是对象，已放弃写入以免覆盖你的配置",
                path.display()
            ),
        ));
    }
    let pre = hooks
        .as_object_mut()
        .expect("刚保证过是对象")
        .entry("PreToolUse")
        .or_insert_with(|| json!([]));
    if !pre.is_array() {
        return Err(StoryError::invalid(
            "settings.json",
            format!(
                "{} 里的 hooks.PreToolUse 不是数组，已放弃写入以免覆盖你的配置",
                path.display()
            ),
        ));
    }
    let list = pre.as_array_mut().expect("刚保证过是数组");
    let mut upgraded = false;
    for item in list.iter_mut() {
        let ours = item
            .get("hooks")
            .and_then(|h| h.as_array())
            .map(|arr| {
                arr.iter().any(|h| {
                    h.get("command")
                        .and_then(|c| c.as_str())
                        .map(|c| c.contains("guard-agent-dir.sh"))
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false);
        if ours {
            // 老版本写过 `Write|Edit|MultiEdit`，这里就地升级（幂等）。
            item["matcher"] = json!("Write|Edit|MultiEdit|Bash");
            upgraded = true;
        }
    }
    if !upgraded {
        list.push(entry);
    }

    let text = serde_json::to_string_pretty(&root)
        .map_err(|e| StoryError::internal(format!("settings.json 序列化失败: {e}")))?;
    atomic_write::write_atomic(&path, text.as_bytes())
}

/// hook 上次运行时是否发现自己没有 python（也就是「硬隔离没生效」）。
///
/// bash 转发层找不到解释器就 exit 0 放行——这是为了不把作者卡死；但这样一来
/// 界面上的「已隔离」就是假的。ensure 之后调用它，拿到 Some 就往引擎日志里告警。
pub fn isolation_degraded(book_root: &Path) -> Option<String> {
    let marker = book_root.join(NEEDS_PYTHON_REL);
    if !marker.exists() {
        return None;
    }
    Some(format!(
        "目录硬隔离没生效：hook 跑的时候找不到 python（{} 存在）。装上 python 或在设置里指定路径后，写手才会被真正限制在自己的目录里。",
        NEEDS_PYTHON_REL
    ))
}

/// 这轮跑完的越界检查：`produced` 是本轮新写/改过的相对路径（`设定/人物.md` 这种）。
pub fn check_writes(book_root: &Path, allowed_dir: &str, produced: &[String]) -> Vec<String> {
    let allowed = allowed_dir.trim().trim_matches('/').trim_matches('\\');
    if allowed.is_empty() {
        return Vec::new();
    }
    let prefix = format!("{allowed}/");
    produced
        .iter()
        .filter(|rel| {
            let rel = rel.replace('\\', "/");
            let in_allowed = rel == allowed || rel.starts_with(&prefix);
            // 记忆/ 的写豁免以前只在调用点（commands/master.rs）过滤，
            // check_writes 自己不知道这条规则——任何新调用点都会把写记忆
            // 误报成越权。规则只留一处：和 CLI hook 的 is_memory_exempt 同源。
            !(in_allowed || memory_write_exempt(&rel))
        })
        .cloned()
        .collect::<Vec<_>>()
        .into_iter()
        .inspect(|rel| {
            eprintln!("[storydesk] 目录越权（{}）：{}", book_root.display(), rel);
        })
        .collect()
}

/// 滚动摘要索引（归编排器所有，见 `services/memory.rs`）。
pub const SUMMARY_INDEX_REL: &str = "记忆/摘要索引.md";

/// `记忆/` 下的写权限：允许新增文件，但 `记忆/摘要索引.md` 只有编排器能写。
///
/// 以前对 `记忆/` 整个目录放行，理由是「写手每章要更新摘要索引」；摘要改由
/// 编排器在验收后统一 append 之后，这条放行就把唯一一份跨章记忆交给了模型去改，
/// 它被整文件重写时没有任何一道闸能发现。返回 true 表示「不算越界」。
///
/// 与 CLI hook（[`PY`] 里的 `is_memory_exempt`）必须保持同一套规则：
/// Rust 放行而 hook 拦截，写手写记忆文件就会收到 exit 2（越权），是两个闸互相矛盾。
pub fn memory_write_exempt(rel: &str) -> bool {
    let rel = rel.replace('\\', "/");
    let rel = rel.strip_prefix("./").unwrap_or(&rel);
    // 绝对路径与路径穿越都不收：`记忆/../正文/第001章.md` 落在正文里，
    // 放行它就等于让任何角色改别人的正文。
    if rel.starts_with('/')
        || rel
            .split('/')
            .any(|seg| seg.is_empty() || seg == "." || seg == "..")
    {
        return false;
    }
    rel.starts_with("记忆/") && rel != SUMMARY_INDEX_REL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_writes_flags_other_dirs() {
        let produced = vec![
            "大纲/细纲_第001章.md".to_string(),
            "设定/人物.md".to_string(),
            "大纲".to_string(),
        ];
        let bad = check_writes(Path::new("C:/book"), "大纲", &produced);
        assert_eq!(bad, vec!["设定/人物.md".to_string()]);
        assert!(check_writes(Path::new("C:/book"), "", &produced).is_empty());
    }

    /// check_writes 自己应用记忆豁免：调用点不用再手动 filter 一遍。
    #[test]
    fn check_writes_exempts_memory_dir_but_still_flags_the_rest() {
        let produced = vec![
            "记忆/第009章要点.md".to_string(),
            "记忆\\子目录\\笔记.md".to_string(),
            "记忆/摘要索引.md".to_string(),
            "正文/第009章_旧稿.md".to_string(),
        ];
        // 允许目录设成「大纲」，正文与记忆索引都该被拦，记忆正文要点该被豁免。
        let bad = check_writes(Path::new("C:/book"), "大纲", &produced);
        assert_eq!(
            bad,
            vec![
                "记忆/摘要索引.md".to_string(),
                "正文/第009章_旧稿.md".to_string()
            ],
            "写记忆文件不该误报，摘要索引与正文仍要拦"
        );
        // 反向：允许目录自己的写照样放行，豁免没有把别的目录一起放开。
        assert!(check_writes(
            Path::new("C:/book"),
            "正文",
            &["正文/第009章.md".to_string()]
        )
        .is_empty());
    }

    /// hook 找不到 python 时留标记，Rust 侧据此告警「隔离没生效」。
    #[test]
    fn missing_python_is_reported_instead_of_silently_passing() {
        assert!(SH.contains(".guard-needs-python"), "hook 模板里没有降级标记");
        let tmp = std::env::temp_dir().join("storydesk-degraded-test");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join(".claude/hooks")).unwrap();
        assert!(isolation_degraded(&tmp).is_none());
        std::fs::write(tmp.join(NEEDS_PYTHON_REL), b"").unwrap();
        assert!(isolation_degraded(&tmp).is_some());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 记忆/ 只放开「新增文件」；摘要索引改写要被收走。
    #[test]
    fn memory_dir_allows_new_files_but_not_the_summary_index() {
        assert!(memory_write_exempt("记忆/第009章要点.md"));
        assert!(memory_write_exempt("记忆\\子目录\\笔记.md"));
        assert!(!memory_write_exempt(SUMMARY_INDEX_REL));
        assert!(
            !memory_write_exempt("记忆\\摘要索引.md"),
            "反斜杠路径同样要拦住"
        );
        assert!(!memory_write_exempt("正文/第009章_旧稿.md"));
    }

    /// 路径穿越不能借 `记忆/` 的豁免去写别的目录。
    #[test]
    fn memory_exempt_rejects_traversal_and_absolute_paths() {
        assert!(!memory_write_exempt("记忆/../正文/第001章_旧稿.md"));
        assert!(!memory_write_exempt("记忆/./第009章要点.md"));
        assert!(!memory_write_exempt("记忆//第009章要点.md"));
        assert!(!memory_write_exempt("/记忆/第009章要点.md"));
        assert!(!memory_write_exempt("C:/book/记忆/第009章要点.md"));
        assert!(!memory_write_exempt("记忆"));
        // 相对路径（带 `./` 前缀）指向的是合法记忆文件，仍然算豁免。
        assert!(memory_write_exempt("./记忆/第009章要点.md"));
    }

    /// 两套闸必须是同一套规则：CLI hook 里得能看到同一条记忆豁免。
    #[test]
    fn cli_hook_carries_the_same_memory_rule() {
        assert!(
            PY.contains("def is_memory_exempt("),
            "hook 里缺少记忆豁免实现"
        );
        assert!(PY.contains("SUMMARY_INDEX_REL = \"记忆/摘要索引.md\""));
        assert!(
            PY.contains("if is_memory_exempt(real, allowed):"),
            "hook 的放行分支没有接上记忆豁免"
        );
    }

    /// 老书里的 matcher 是 `Write|Edit|MultiEdit`，PowerShell 写盘根本不会触发 hook
    /// （第 037 章就这么被写坏的）。`ensure` 必须就地升级成带 Bash 的版本，而且幂等。
    #[test]
    fn ensure_upgrades_the_old_hook_matcher_to_include_bash() {
        let root = std::env::temp_dir().join(format!(
            "storydesk-guard-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let settings = root.join(".claude").join("settings.json");
        std::fs::write(
            &settings,
            r#"{"hooks":{"PreToolUse":[{"matcher":"Write|Edit|MultiEdit","hooks":[{"type":"command","command":"bash .claude/hooks/guard-agent-dir.sh"}]},{"matcher":"Read","hooks":[{"type":"command","command":"echo hi"}]}]}}"#,
        )
        .unwrap();

        ensure(&root).unwrap();
        let first: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let list = first["hooks"]["PreToolUse"].as_array().unwrap().clone();
        let ours: Vec<&serde_json::Value> = list
            .iter()
            .filter(|item| {
                item["hooks"][0]["command"]
                    .as_str()
                    .map(|c| c.contains("guard-agent-dir.sh"))
                    .unwrap_or(false)
            })
            .collect();
        assert_eq!(ours.len(), 1, "guard 条目不能重复");
        assert_eq!(
            ours[0]["matcher"].as_str(),
            Some("Write|Edit|MultiEdit|Bash")
        );
        assert_eq!(list.len(), 2, "别人加的 PreToolUse 条目不能被删掉");
        assert!(root.join(HOOK_REL).exists() && root.join(HOOK_PY_REL).exists());

        // 再装一次：条目数、matcher 都不变。
        ensure(&root).unwrap();
        let second: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        let list2 = second["hooks"]["PreToolUse"].as_array().unwrap();
        assert_eq!(list2.len(), 2);
        assert_eq!(
            list2[0]["matcher"].as_str(),
            Some("Write|Edit|MultiEdit|Bash")
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 解析失败时一个字节都不许写。
    ///
    /// 旧实现把「读不到」和「解析失败」都当成空对象，随后用「只有一个 PreToolUse」的对象
    /// 整份覆盖：用户手改的 permissions、别人加的 hooks 就这么没了（BOM / 尾逗号 / 注释都会触发）。
    /// 同时确认带 BOM 的合法 JSON 能正常合并——记事本存成 UTF-8 with BOM 是常态。
    #[test]
    fn broken_settings_json_is_never_overwritten() {
        let root = std::env::temp_dir().join(format!(
            "storydesk-guard-broken-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join(".claude")).unwrap();
        let settings = root.join(".claude").join("settings.json");

        // 1) 语法坏掉（尾逗号）：必须报错，原文件保持不变。
        let broken = r#"{"permissions":{"allow":["Bash"]},}"#;
        std::fs::write(&settings, broken).unwrap();
        assert!(register_in_settings(&root).is_err(), "坏 JSON 必须报错");
        assert_eq!(
            std::fs::read_to_string(&settings).unwrap(),
            broken,
            "解析失败时不许改写用户的 settings.json"
        );

        // 2) 合法但带 BOM：要能剥掉 BOM 正常合并，并保住用户原有配置。
        let mut bom = vec![0xEFu8, 0xBB, 0xBF];
        bom.extend_from_slice(br#"{"permissions":{"allow":["Bash"]}}"#);
        std::fs::write(&settings, &bom).unwrap();
        register_in_settings(&root).unwrap();
        let after: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        assert_eq!(
            after["permissions"]["allow"][0].as_str(),
            Some("Bash"),
            "用户原有配置必须保留"
        );
        assert_eq!(
            after["hooks"]["PreToolUse"]
                .as_array()
                .map(|a| a.len())
                .unwrap_or(0),
            1,
            "hook 条目要被加进去"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// hook 里必须真的有拦 PowerShell 写盘的那段，且认得书里的内容目录。
    #[test]
    fn hook_py_blocks_powershell_writes_into_the_book() {
        for needle in [
            "def forbidden_bash(",
            "WRITE_VERBS",
            "\"Set-Content\"",
            "\"[IO.File]::WriteAll\"",
            "CONTENT_DIRS",
            "cp936",
            "if command and forbidden_bash(command):",
        ] {
            assert!(PY.contains(needle), "hook 里缺少 {}", needle);
        }
    }
}
