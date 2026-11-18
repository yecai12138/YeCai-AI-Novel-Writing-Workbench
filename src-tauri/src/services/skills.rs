//! 预装开源写作 skill：把 oh-story-claudecode（MIT）的两份 SKILL.md 释放到
//! `<书>/.claude/skills/<名字>/SKILL.md`，宿主 CLI（Claude Code / opencode）能直接读到。
//!
//! 装的是：
//! - `story-deslop`：网文去AI味（表达 / 用笔 / 故事三层诊断与修复）；
//! - `story-long-analyze`：长篇拆文（黄金三章、逐章摘要、情绪与节奏拆解）。
//!
//! 幂等规则与 [`dir_guard`] 一致：文件已存在就**不覆盖**（用户可能改过），
//! 失败不致命——skill 缺失只是少一份方法论，不该卡住整条流水线。

use std::path::Path;

/// `(skill 名, 内置内容)`。内容编译期打进二进制，运行不依赖网络。
const SKILLS: [(&str, &str); 2] = [
    (
        "story-deslop",
        include_str!("../../resources/skills/story-deslop.md"),
    ),
    (
        "story-long-analyze",
        include_str!("../../resources/skills/story-long-analyze.md"),
    ),
];

/// 把两份 skill 释放进 `<书>/.claude/skills/`；返回本次实际新装的路径（相对书根）。
pub fn ensure(book_root: &Path) -> Vec<String> {
    let mut installed = Vec::new();
    for (name, body) in SKILLS {
        let path = book_root
            .join(".claude")
            .join("skills")
            .join(name)
            .join("SKILL.md");
        // 已有就不动：用户改过的内容比内置版本更有权活着。
        if path.is_file() {
            continue;
        }
        if std::fs::create_dir_all(path.parent().expect("skill 路径必有父目录")).is_err() {
            continue;
        }
        if std::fs::write(&path, body.as_bytes()).is_ok() {
            installed.push(format!(".claude/skills/{name}/SKILL.md"));
        }
    }
    installed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn installs_once_and_never_overwrites() {
        let root =
            std::env::temp_dir().join(format!("storydesk-skills-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let first = ensure(&root);
        assert_eq!(first.len(), 2, "{first:?}");
        let deslop = root.join(".claude/skills/story-deslop/SKILL.md");
        assert!(deslop.is_file());
        // 内容非空且带着 frontmatter 的名字。
        let body = fs::read_to_string(&deslop).unwrap();
        assert!(body.contains("story-deslop"), "内容应含 skill 名");

        // 用户手改一份后重跑：不许覆盖，也不许重复报「新装」。
        fs::write(&deslop, "我自己的版本").unwrap();
        let second = ensure(&root);
        assert!(second.is_empty(), "{second:?}");
        assert_eq!(fs::read_to_string(&deslop).unwrap(), "我自己的版本");

        let _ = fs::remove_dir_all(&root);
    }
}
