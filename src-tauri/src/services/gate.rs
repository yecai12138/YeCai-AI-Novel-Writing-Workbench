//! 门禁脚本调用：`storyctl.py`（章节体检/提交）与 `build_architect_brief.py`（任务包）。
//!
//! 两条硬规则（阶段 1 调研得出的坑）：
//! 1. **stdout 必须按字节读**再 `String::from_utf8_lossy`。Windows 控制台默认 cp936/cp1252，
//!    直接 `read_to_string` 会在中文输出上炸 `invalid utf-8`；脚本自己也是写 UTF-8 字节流
//!    （`storyctl.py` 用 `sys.stdout.buffer` + `ensure_ascii=False`）。
//! 2. **脚本位置不由我们假设**。优先认 oh-story 装在宿主里的原版（配置覆盖 → 工作区 →
//!    用户主目录）；一份都找不到时，把**内置**的脚本释放到 `<工作区>/.storydesk/tools/`
//!    再用（阶段 12：不再要求用户先装一遍 oh-story）。

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::models::dto::{GateInfo, GateOutput, PostCheck, PostCheckIssue, TaskPacket, GateRemedy, GateVerdict};
use crate::models::StoryError;
use crate::services::atomic_write::write_atomic;
use crate::services::env_check::which_exe;
use crate::services::snapshot::sha1_hex;

/// 打进 exe 的脚本（`src-tauri/assets/tools/`，调用契约与 gate.rs 对齐）。
const BUNDLED_STORYCTL: &str = include_str!("../../assets/tools/storyctl.py");
const BUNDLED_BRIEF: &str = include_str!("../../assets/tools/build_architect_brief.py");

/// 定位到的脚本链。
#[derive(Debug, Clone, Default)]
pub struct Scripts {
    /// python 可执行文件
    pub python: Option<PathBuf>,
    /// python 前置参数（`py -3` 时是 `["-3"]`）
    pub python_args: Vec<String>,
    pub storyctl: Option<PathBuf>,
    pub brief: Option<PathBuf>,
}

impl Scripts {
    /// 前置门禁是否可跑（python + storyctl 都在）。
    pub fn gate_ready(&self) -> bool {
        self.python.is_some() && self.storyctl.is_some()
    }

    /// 任务包生成是否可跑。
    pub fn brief_ready(&self) -> bool {
        self.python.is_some() && self.brief.is_some()
    }
}

/// 找 python：python → python3 → py -3（与 env_check 的策略一致）。
fn find_python() -> (Option<PathBuf>, Vec<String>) {
    for name in ["python", "python3"] {
        if let Some(p) = which_exe(name) {
            return (Some(p), Vec::new());
        }
    }
    if let Some(p) = which_exe("py") {
        return (Some(p), vec!["-3".to_string()]);
    }
    // PATH 没有：认应用自动下载的嵌入式 Python（设置 → 环境自检 一键下载）。
    if let Some(p) = crate::services::runtime_setup::locate_python() {
        return (Some(p), Vec::new());
    }
    (None, Vec::new())
}

/// 在 `root` 下的 `.claude/skills/*/scripts/<file>` 与 `.agents/skills/*/scripts/<file>` 里找脚本。
fn scan_skill_dirs(root: &Path, file: &str) -> Option<PathBuf> {
    for base in [".claude/skills", ".agents/skills", "skills"] {
        let dir = root.join(base);
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let candidate = entry.path().join("scripts").join(file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// 读 `.storydesk/config.json` 里的 `storyctlPath` 覆盖项。
fn config_override(workspace_root: &Path) -> Option<PathBuf> {
    let cfg = workspace_root.join(".storydesk").join("config.json");
    let text = std::fs::read_to_string(cfg).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let raw = value.get("storyctlPath")?.as_str()?.trim();
    if raw.is_empty() {
        return None;
    }
    let path = PathBuf::from(raw);
    if path.is_file() {
        Some(path)
    } else {
        None
    }
}

/// 把内置脚本释放到 `<工作区>/.storydesk/tools/`。
///
/// 已经存在同名文件就**不覆盖**（用户可能放了自己的版本）；写失败如实返回 None。
fn release_bundled(workspace_root: &Path, file: &str, body: &str) -> Option<PathBuf> {
    let dir = workspace_root.join(".storydesk").join("tools");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(file);
    if path.is_file() {
        return Some(path);
    }
    std::fs::write(&path, body).ok()?;
    Some(path)
}

/// 定位整条脚本链。
pub fn resolve(workspace_root: &Path) -> Scripts {
    let (python, python_args) = find_python();

    // 内置脚本只在工作区还没有任何 storyctl 时释放（不顶掉用户自己装的）。
    let bundled_storyctl = release_bundled(workspace_root, "storyctl.py", BUNDLED_STORYCTL);
    let bundled_brief = release_bundled(workspace_root, "build_architect_brief.py", BUNDLED_BRIEF);

    let storyctl = config_override(workspace_root)
        .or_else(|| scan_skill_dirs(workspace_root, "storyctl.py"))
        .or_else(|| {
            std::env::var_os("USERPROFILE")
                .or_else(|| std::env::var_os("HOME"))
                .map(PathBuf::from)
                .and_then(|home| scan_skill_dirs(&home, "storyctl.py"))
        })
        .or_else(|| bundled_storyctl);

    // build_architect_brief.py 与 storyctl.py 同在 story-long-write/scripts/ 下。
    let brief = storyctl
        .as_ref()
        .and_then(|p| p.parent())
        .map(|dir| dir.join("build_architect_brief.py"))
        .filter(|p| p.is_file())
        .or_else(|| scan_skill_dirs(workspace_root, "build_architect_brief.py"))
        .or_else(|| bundled_brief);

    Scripts {
        python,
        python_args,
        storyctl,
        brief,
    }
}

/// 探测结果（设置页/AI 面板展示）。
pub fn info(workspace_root: &Path) -> GateInfo {
    let s = resolve(workspace_root);
    let mut notes: Vec<String> = Vec::new();

    if s.python.is_none() {
        notes.push("未找到 python（安装 Python 3.9+ 并勾选 Add to PATH）".to_string());
    }
    match (&s.storyctl, &s.python) {
        (None, _) => notes.push(
            "storyctl.py 释放失败：无法写入 工作区/.storydesk/tools/。可手动把 storyctl.py 放进该目录，或在 .storydesk/config.json 写 {\"storyctlPath\": \"<绝对路径>\"}"
                .to_string(),
        ),
        (Some(p), Some(_)) => notes.push(format!("storyctl.py: {}", p.display())),
        _ => {}
    }
    match (&s.brief, &s.python) {
        (None, Some(_)) => notes.push("build_architect_brief.py 释放失败：无法写入 工作区/.storydesk/tools/（任务包无法生成，可用正文直写模式）".to_string()),
        (Some(p), _) => notes.push(format!("build_architect_brief.py: {}", p.display())),
        _ => {}
    }

    GateInfo {
        ok: s.gate_ready(),
        python: s.python.as_ref().map(|p| p.display().to_string()),
        storyctl: s.storyctl.as_ref().map(|p| p.display().to_string()),
        brief: s.brief.as_ref().map(|p| p.display().to_string()),
        detail: notes.join("；"),
    }
}

fn hide_window(cmd: &mut Command) -> &mut Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    cmd
}

/// 一次 python 脚本调用的原始产物（字节流 + 退出状态）。
struct GateProcessOutput {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    status: Option<std::process::ExitStatus>,
}

/// 跑一次 python 脚本；stdout/stderr 一律按字节取回再 lossy 解码。
fn run_python(
    scripts: &Scripts,
    script: &Path,
    args: &[String],
    cwd: Option<&Path>,
) -> Result<GateOutput, StoryError> {
    let Some(python) = scripts.python.as_ref() else {
        return Err(StoryError::env_missing(
            "python",
            "未找到 Python 3。oh-story 的门禁脚本（storyctl.py / build_architect_brief.py）由 Python 执行。",
            "安装 Python 3.9+（https://www.python.org/downloads/），勾选 Add python.exe to PATH，确认 `python --version` 可用。",
        ));
    };

    let mut cmd = Command::new(python);
    cmd.args(&scripts.python_args)
        .arg(script)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // 脚本内部按 UTF-8 写字节流，这里也要求解释器别按控制台代码页转码。
        .env("PYTHONIOENCODING", "utf-8")
        .env("PYTHONUTF8", "1");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    hide_window(&mut cmd);

    // 加超时：storyctl.py 若阻塞（死锁、等 stdin、编码炸了）会无限卡住，
    // 整批派发就不动了。给 60 秒兜底，超时直接 kill 子进程并报错。
    //
    // 这里是同步函数（调用方不在 async 上下文里），所以不用 tokio::time::timeout：
    // 直接 spawn + 两个线程抽干 stdout/stderr（避免管道写满互相等待），
    // 主线程轮询 try_wait 计时，超时就 kill。
    const PYTHON_TIMEOUT: Duration = Duration::from_secs(60);
    let mut child = cmd
        .spawn()
        .map_err(|e| StoryError::internal(format!("启动 {} 失败: {e}", script.display())))?;

    let mut child_stdout = child.stdout.take();
    let mut child_stderr = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = child_stdout.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = child_stderr.as_mut() {
            let _ = pipe.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = std::time::Instant::now() + PYTHON_TIMEOUT;
    let mut timed_out = false;
    let mut exit_status: Option<std::process::ExitStatus> = None;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                exit_status = Some(status);
                break;
            }
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(StoryError::internal(format!(
                    "等待 {} 退出失败: {e}",
                    script.display()
                )));
            }
        }
    }

    let stdout_bytes = stdout_reader.join().unwrap_or_default();
    let stderr_bytes = stderr_reader.join().unwrap_or_default();

    if timed_out {
        return Err(StoryError::internal(format!(
            "{} 执行超时（>{} 秒），疑似脚本阻塞或死锁，已强制结束",
            script.display(),
            PYTHON_TIMEOUT.as_secs()
        )));
    }

    let out = GateProcessOutput {
        stdout: stdout_bytes,
        stderr: stderr_bytes,
        status: exit_status,
    };

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let code = out.status.and_then(|status| status.code()).unwrap_or(-1);

    // 脚本自己崩了（缺相邻模块、参数不兼容）不是「体检不通过」：
    // 必须报成错误，否则界面会把 Python traceback 当成 hook 拦截文案展示给用户。
    if code != 0 && stderr.contains("Traceback (most recent call last)") {
        let last = stderr
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .next_back()
            .unwrap_or("")
            .to_string();
        return Err(StoryError::internal(format!(
            "{} 运行失败（可能缺相邻脚本或版本不匹配）：{last}",
            script.display()
        )));
    }

    Ok(GateOutput {
        ok: code == 0,
        code,
        stdout,
        stderr,
    })
}

// ---------------------------------------------------------------- 前置门禁（写正文之前）

/// hook 的细纲字数门槛（`guard-outline-before-prose.sh` 默认 300）。
const OUTLINE_MIN_CHARS: usize = 300;

/// 写正文之前该拦的两件事：**细纲在不在、够不够**，以及**细纲声称的事件成不成立**。
///
/// 不能拿 `storyctl.py chapter check` 当前置门禁：它校验的是**已经写出来的正文**
/// （先报 `chapter 1 must have exactly one outline file`，细纲补上后立刻改报
/// `没有正文`），语义上是「写完/提交前体检」。所以这里按 oh-story 的前置语义分两条路：
/// 1. 项目里装了 oh-story 的 hook 脚本 → 按 Claude Code 的协议真跑一遍
///    （stdin JSON / exit 2 + stderr），与宿主里的行为完全一致；
/// 2. 没装脚本 → 本地按同一套规则检查：`大纲/细纲_第NNN章.md` 恰好一份且去空白 ≥ 300 字。
///
/// 细纲过了之后**再跑事件检查**（[`event_precheck`]）。事件检查不走脚本：脚本可能没装 bash、
/// 可能被作者改过、也可能只查细纲字数——把「事件成不成立」交给它，等于留一个可绕过的口子。
pub fn precheck_prose(
    workspace_root: &Path,
    book_root: &Path,
    chapter: u32,
) -> Result<GateOutput, StoryError> {
    let outline = match find_hook_script(workspace_root, book_root) {
        Some(script) => match run_hook(&script, book_root, chapter)? {
            Some(out) => out,
            // 脚本自己 fail-open（exit 0）时不能就此放行，还要过下面的事件检查。
            None => local_outline_check(book_root, chapter),
        },
        None => local_outline_check(book_root, chapter),
    };

    // 细纲都没过就只报细纲：一次只修最上游的那件事，别让人同时读两条不通的错。
    if !outline.ok {
        return Ok(outline);
    }

    // 事件检查**不交给 hook 脚本**：脚本可能没装 bash、可能被改过、也可能只查细纲字数。
    // 这里查的全是细纲自己声明的字段，确定性判断，不需要模型参与。
    if let Some(blocked) = event_precheck(book_root, chapter) {
        return Ok(blocked);
    }

    Ok(outline)
}

/// 事件层前置检查：细纲声称属于哪个事件，就得真有那张卡，且卡还开着、还排了这一章。
///
/// 「卡不在」不算硬错误：本书没有事件层，或模型自己编了个事件号，写出来也只是少了
/// 一本事件账，不会把账记错。只有「卡在、但已结算」「卡在、但没排这一章」才拦——
/// 那两条才是写下去必然对不上账的情形，而且都给得出具体出口。
fn event_precheck(book_root: &Path, chapter: u32) -> Option<GateOutput> {
    let declared = crate::services::context::declared_event(book_root, chapter)?;
    let Some(card) = crate::services::context::find_event_card(book_root, &declared) else {
        // 缺卡不拦。两种情形都只提示：
        // 1. 书里一张事件卡都没有——老书形态，本来就是一章一事，没有账本可对不上；
        // 2. 有事件卡，但模型照自己的编号习惯写了「事件 301」——这行字段只是一句声明，
        //    挡下去会让整条流水停在第一章（真实事故：第 301 章起全部被拦，330 章的细纲永远排不到）。
        // 硬拦只留给「卡在、但状态或章型分工明确冲突」的两条。
        let no_cards = crate::services::context::all_event_cards(book_root).is_empty();
        let why = if no_cards {
            "本书还没有任何事件卡"
        } else {
            "大纲/事件/ 下没有这张卡"
        };
        return Some(event_pass(format!(
            "事件层提示：第 {chapter} 章声明的「事件 {declared}」不在事件层里（{why}）——本轮不拦，按一章一事写正文；\
             要真挂到事件层，就让架构师补 大纲/事件/事件_{declared}_标题.md，或者把这章细纲的「所属事件」改成「无」。"
        )));
    };
    let label = format!("事件 {}（{}）", card.id, card.title);

    if matches!(card.status, crate::models::event::EventStatus::Settled) {
        return Some(event_block(format!(
            "⛔ 写正文被拦截：第 {chapter} 章声称属于{label}，但该事件已结算，不能再往里加章。\
             要么把这章挂到下一张事件卡，要么先把账本里误结的项改回来。"
        )));
    }

    let planned = card.role_of(chapter).is_some()
        || card
            .effective_span()
            .is_some_and(|(start, end)| (start..=end).contains(&chapter));
    if !planned {
        return Some(event_block(format!(
            "⛔ 写正文被拦截：第 {chapter} 章声称属于{label}，但事件卡的「章型分工」里没有这一章。\
             先把这一章补进章型分工（章型只能写 起/推/转/结），或者把这章细纲的「所属事件」改成「无」。"
        )));
    }

    None
}

/// 事件层的提示性放行：ok 为真（照写正文），stdout 是给作者与日志看的一句话。
fn event_pass(text: String) -> GateOutput {
    GateOutput {
        ok: true,
        code: 0,
        stdout: text,
        stderr: String::new(),
    }
}

fn event_block(text: String) -> GateOutput {
    GateOutput {
        ok: false,
        code: 2,
        stdout: text,
        stderr: String::new(),
    }
}

/// 项目里的 `guard-outline-before-prose.sh`（工作区或书目录下都认）。
fn find_hook_script(workspace_root: &Path, book_root: &Path) -> Option<PathBuf> {
    const REL: &str = "hooks/guard-outline-before-prose.sh";
    let candidates = [
        workspace_root.join(".claude").join(REL),
        workspace_root.join(".agents").join(REL),
        book_root.join(".claude").join(REL),
    ];
    candidates.into_iter().find(|path| path.is_file())
}

/// 按 Claude Code 的 PreToolUse 协议跑 hook：stdin JSON、exit 2 = 拦截、stderr = 文案。
/// 返回 `None` 表示 hook 自己 fail-open（exit 0），需要回落到本地检查。
fn run_hook(
    script: &Path,
    book_root: &Path,
    chapter: u32,
) -> Result<Option<GateOutput>, StoryError> {
    let Some(bash) = bash_exe() else {
        return Ok(None);
    };
    // hook 只看「要写哪个路径」，给一个该章的路径即可。
    let target = book_root
        .join("正文")
        .join(format!("第{chapter:03}章_预检.md"));
    let payload = format!(
        r#"{{"session_id":"storydesk","cwd":{},"hook_event_name":"PreToolUse","tool_name":"Write","tool_input":{{"file_path":{}}}}}"#,
        json_string(&book_root.display().to_string()),
        json_string(&target.display().to_string()),
    );

    let mut cmd = Command::new(bash);
    cmd.arg(script)
        .current_dir(book_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hide_window(&mut cmd);

    let mut child = cmd
        .spawn()
        .map_err(|e| StoryError::internal(format!("启动 bash 跑 hook 失败: {e}")))?;
    if let Some(mut stdin) = child.stdin.take() {
        use std::io::Write;
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child
        .wait_with_output()
        .map_err(|e| StoryError::internal(format!("等待 hook 结束失败: {e}")))?;

    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    match code {
        // 拦截：stderr 就是 hook 的原话。
        2 => Ok(Some(GateOutput {
            ok: false,
            code,
            stdout,
            stderr,
        })),
        0 => Ok(None),
        _ => Err(StoryError::internal(format!(
            "hook 异常退出（{code}）：{}",
            stderr.lines().next().unwrap_or("").trim()
        ))),
    }
}

/// 把一次门禁拦下的结果分级：能靠插一步修好的，不该让整轮编排停在这里。
///
/// 分级只看**门禁自己写的那句话**（stdout 优先，hook 脚本走 stderr）——这些文案
/// 是本进程内 `local_outline_check` / `event_precheck` 生成的，可控；来自外部 hook
/// 脚本的未知文案一律按 [`GateVerdict::Stop`] 处理：看不懂的拦截不能自动放行。
pub fn classify_block(chapter: u32, out: &GateOutput) -> (GateVerdict, GateRemedy) {
    use crate::models::dto::{GateRemedy, GateVerdict};
    if out.ok {
        return (GateVerdict::Pass, GateRemedy::None);
    }
    let text = if out.stdout.trim().is_empty() {
        out.stderr.as_str()
    } else {
        out.stdout.as_str()
    };
    // 「细纲不止一份」是人工整理问题：自动补一步只会再造一份，越补越乱。
    if text.contains("不止一份") {
        return (GateVerdict::Stop, GateRemedy::None);
    }
    // 「读不出来」是环境问题（文件被占用 / 不是 UTF-8），不是缺内容：
    // 这时候补写一份等于把作者写好的原稿顶掉，只能停下等人工处理。
    if text.contains("读不出来") {
        return (GateVerdict::Stop, GateRemedy::None);
    }
    let about_outline = text.contains("细纲");
    let missing = text.contains("没有细纲")
        || text.contains("不存在")
        || text.contains("是空的")
        || text.contains("先写这一章细纲");
    if about_outline && missing {
        return (GateVerdict::Repairable, GateRemedy::WriteOutline(chapter));
    }
    // 事件层：卡已结算 / 卡里没排这一章——插一步改事件卡就能续上。
    if text.contains("事件") {
        return (GateVerdict::Repairable, GateRemedy::FixEventCard(chapter));
    }
    (GateVerdict::Stop, GateRemedy::None)
}

/// `大纲/` 顶层以「细纲」开头、章号命中本章的文件。
///
/// 只用于「同名多份」这条人工整理提示：**「有没有细纲」不靠它判断**，
/// 统一走 [`crate::services::context::outline_path`]。
fn top_level_outline_files(book_root: &Path, chapter: u32) -> Vec<PathBuf> {
    let dir = book_root.join("大纲");
    let mut hits: Vec<PathBuf> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.starts_with("细纲") || !name.ends_with(".md") {
                continue;
            }
            if crate::services::context::chapter_in_name(name) == Some(chapter) {
                hits.push(path);
            }
        }
    }
    hits
}

fn local_outline_check(book_root: &Path, chapter: u32) -> GateOutput {
    // 存在性判定必须和上下文层用同一套：context::outline_path 认三种形态——
    // 「大纲/细纲索引.md 指向的文件」、「大纲/细纲_第NNN章.md」，以及合并卷里的
    // 「## 第N章」段落。这里过去只扫顶层「细纲_」开头的文件，于是用合并卷维护细纲的书
    // 每章都被判「没有细纲」，然后自动补一步 WriteOutline 生成第二份细纲：此后 gate 认
    // 新文件、context 认卷文，两个模块长期读不同的细纲，作者的卷文件被彻底晾在一边。
    let name_of = |path: &PathBuf| -> String {
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string()
    };

    // 同名多份：优先认标准命名，其余当历史副本忽略并说明；连标准命名都没有才报出来让人删。
    // 以前只要不是恰好一份就笼统报「没有细纲，或者细纲不止一份」，人看不出到底缺的还是多的。
    // 这一段必须在 outline_path 之前判：它只负责选「一份」，选不出来时会返回「没有」，
    // 那样多重副本就会被误报成缺细纲，接着又自动补写第三份。
    let hits = top_level_outline_files(book_root, chapter);
    let canonical = [format!("细纲_第{chapter:03}章.md"), format!("细纲_第{chapter}章.md")];
    let canonical_hit = hits
        .iter()
        .find(|p| canonical.iter().any(|c| name_of(p) == *c))
        .cloned();
    if hits.len() > 1 && canonical_hit.is_none() {
        let names: Vec<String> = hits.iter().map(name_of).collect();
        return GateOutput {
            ok: false,
            code: 2,
            stdout: format!(
                "这次没查成：第 {chapter} 章细纲不止一份（{}），且没有一份叫 细纲_第{chapter:03}章.md。先删到只剩一份再写正文。",
                names.join("、")
            ),
            stderr: String::new(),
        };
    }

    // 顶层只有一份但不是标准命名、字数又没到合并卷的扫描门槛时，仍然认这一份：
    // 否则会从「细纲是空的」退化成「没有细纲」，把「太短」误报成「缺文件」。
    let source = crate::services::context::outline_path(book_root, chapter).or_else(|| {
        if hits.len() == 1 {
            Some(hits[0].clone())
        } else {
            None
        }
    });
    let Some(source) = source else {
        return GateOutput {
            ok: false,
            code: 2,
            stdout: format!(
                "这次没查成：第 {chapter} 章没有细纲——大纲/细纲_第{chapter:03}章.md 不存在，先写这一章细纲再写正文。"
            ),
            stderr: String::new(),
        };
    };

    let chosen = canonical_hit.unwrap_or(source);
    let ignored: Vec<String> = hits
        .iter()
        .filter(|p| **p != chosen)
        .map(name_of)
        .collect();

    // 读不出来 ≠ 空的：编码坏了 / 没权限时不能当「细纲是空的」，否则 classify_block
    // 会归成可修复的「空细纲」，编排层于是把作者已经写好的细纲重写一遍。
    // read_text 会剥 UTF-8 BOM，与其它读文本的地方同一口径。
    let body = match crate::services::atomic_write::read_text(&chosen) {
        Ok(text) => text,
        Err(e) => {
            return GateOutput {
                ok: false,
                code: 2,
                stdout: format!(
                    "这次没查成：第 {chapter} 章的细纲文件读不出来（{e}）。已停止，不自动重写细纲。"
                ),
                stderr: String::new(),
            };
        }
    };
    // 与 hook 同口径：不计 `#` 号与空白。
    let chars = body
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '#')
        .count();
    if chars < OUTLINE_MIN_CHARS {
        return GateOutput {
            ok: false,
            code: 2,
            stdout: format!(
                "这次没查成：第 {chapter} 章的细纲是空的（不计 # 号和空白不到 {OUTLINE_MIN_CHARS} 字）。"
            ),
            stderr: String::new(),
        };
    }

    let mut note = format!("第 {chapter} 章细纲已在位（{chars} 字），可以写正文。");
    if !ignored.is_empty() {
        note.push_str(&format!("（忽略了同名副本：{}）", ignored.join("、")));
    }
    GateOutput {
        ok: true,
        code: 0,
        stdout: note,
        stderr: String::new(),
    }
}

fn json_string(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for ch in raw.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn bash_exe() -> Option<PathBuf> {
    which_exe("bash")
        .filter(|p| {
            !p.to_string_lossy()
                .to_ascii_lowercase()
                .contains("system32")
        })
        .or_else(|| {
            let git = which_exe("git")?;
            let candidate = git.parent()?.join("../bin/bash.exe");
            candidate.canonicalize().ok()
        })
        // 应用自动下载的 PortableGit（usr\bin\bash.exe，旁边就是全套 coreutils）
        .or_else(crate::services::runtime_setup::locate_gitbash)
}

/// `storyctl.py chapter check --project <书> --chapter N --plain`
pub fn chapter_check(
    workspace_root: &Path,
    book_root: &Path,
    chapter: u32,
) -> Result<GateOutput, StoryError> {
    let scripts = resolve(workspace_root);
    let Some(storyctl) = scripts.storyctl.clone() else {
        return Err(StoryError::env_missing(
            "storyctl",
            "未找到 storyctl.py，无法做写正文前的章节体检。",
            "在项目根运行 /story-setup（Codex 用 $story-setup），或在 .storydesk/config.json 里指定 storyctlPath。",
        ));
    };

    let args = vec![
        "chapter".to_string(),
        "check".to_string(),
        "--project".to_string(),
        book_root.display().to_string(),
        "--chapter".to_string(),
        chapter.to_string(),
        "--plain".to_string(),
    ];

    run_python(&scripts, &storyctl, &args, Some(workspace_root))
}

// ---------------------------------------------------------------- 任务包缓存

/// 任务包缓存索引（相对书根）。任务包正文由脚本写进 `.storydesk/prompts/`，
/// 这里只记「哪组参数 → 哪份文件 + 生成时的源文件指纹」。
const PACKET_CACHE_REL: &str = ".storydesk/prompts/.packet-cache.json";

/// 会影响任务包内容的源目录（相对书根）。
///
/// 故意**不含** `记忆/` 与 `正文/`：这两个目录每写一章就变，带上它们会让整轮批量任务
/// 的缓存全数失效——而任务包（细纲/卷纲/设定的说明）根本不依赖已完成的正文章节。
const PACKET_SOURCE_DIRS: [&str; 5] = ["设定", "追踪", "大纲", "参考资料", "对标"];

/// 指纹最多扫多少个文件（防止超大的 参考资料/ 拖慢每一步）。
const PACKET_DIGEST_MAX_FILES: usize = 2000;

/// 源目录递归深度上限。
const PACKET_DIGEST_DEPTH: usize = 3;

/// 缓存条目上限，超出按最旧淘汰。
const PACKET_CACHE_MAX: usize = 32;

/// 任务包正文所在目录（相对书根）。
const PACKET_DIR_REL: &str = ".storydesk/prompts";

/// 孤儿任务包的宽限期：这么久没动过才允许删，避免和「正在生成/正在被这一步读取」的包抢文件。
const ORPHAN_GRACE_MS: u64 = 30 * 60 * 1000;

#[derive(Debug, Default, Serialize, Deserialize)]
struct PacketCache {
    #[serde(default)]
    entries: BTreeMap<String, PacketCacheEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PacketCacheEntry {
    /// 任务包文件的绝对路径（文件名由脚本决定，这里只记）。
    path: String,
    /// 任务包文件写入时的 mtime（毫秒）；0 = 拿不到时间，一律当失效。
    mtime: u64,
    /// 生成时的源文件指纹，见 [`packet_sources_digest`]。
    sources: String,
    /// 落缓存的时间（毫秒），淘汰用。
    at: u64,
}

/// 缓存键：任务类型 + 卷 + 章区间，任一不同就是另一份任务包。
pub fn packet_cache_key(task: &str, volume: Option<u32>, chapters: Option<(u32, u32)>) -> String {
    let volume = volume
        .map(|v| v.to_string())
        .unwrap_or_else(|| "-".to_string());
    let chapters = chapters
        .map(|(from, to)| format!("{from}-{to}"))
        .unwrap_or_else(|| "-".to_string());
    format!("{task}|v={volume}|c={chapters}")
}

fn mtime_ms(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// 源文件指纹：`设定/追踪/大纲/参考资料/对标` 下所有 .md 的「路径 + mtime + 大小」。
///
/// 用 mtime 而不是内容哈希：整本书几十万字，每步哈希一遍不值当；mtime+大小已经能
/// 分辨「作者改了设定」与「什么都没变」，而包的正文由脚本决定，误判的代价只是多跑一次。
pub fn packet_sources_digest(book_root: &Path) -> String {
    let mut rows: Vec<String> = Vec::new();
    for dir in PACKET_SOURCE_DIRS {
        walk_packet_sources(book_root, dir, 1, &mut rows);
    }
    rows.sort();
    sha1_hex(rows.join("\n").as_bytes())
}

fn walk_packet_sources(book_root: &Path, rel: &str, depth: usize, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(book_root.join(rel)) else {
        return;
    };
    let mut names: Vec<_> = entries.flatten().collect();
    names.sort_by_key(|e| e.file_name());
    for entry in names {
        if out.len() >= PACKET_DIGEST_MAX_FILES {
            return;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let child = format!("{rel}/{name}");
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            if depth < PACKET_DIGEST_DEPTH {
                walk_packet_sources(book_root, &child, depth + 1, out);
            }
        } else if name.to_ascii_lowercase().ends_with(".md") {
            let mtime = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            out.push(format!("{child}\t{mtime}\t{}", meta.len()));
        }
    }
}

fn cache_index_path(book_root: &Path) -> PathBuf {
    book_root.join(PACKET_CACHE_REL)
}

fn read_packet_cache(book_root: &Path) -> PacketCache {
    let Ok(text) = std::fs::read_to_string(cache_index_path(book_root)) else {
        return PacketCache::default();
    };
    serde_json::from_str(&text).unwrap_or_default()
}

fn write_packet_cache(book_root: &Path, cache: &PacketCache) {
    let Ok(text) = serde_json::to_string_pretty(cache) else {
        return;
    };
    if let Err(e) = write_atomic(&cache_index_path(book_root), text.as_bytes()) {
        // 缓存写不进去只是慢一点，不能影响这一轮的执行。
        eprintln!("[storydesk] 任务包缓存索引写入失败：{e}");
    }
}

/// 查缓存：键相同、源文件指纹相同、任务包文件还在且没被改过才算命中。
fn packet_cache_lookup(book_root: &Path, key: &str, digest: &str) -> Option<PathBuf> {
    let cache = read_packet_cache(book_root);
    let entry = cache.entries.get(key)?;
    if entry.sources != digest {
        return None;
    }
    let path = PathBuf::from(&entry.path);
    if !path.is_file() {
        return None;
    }
    if entry.mtime == 0 || mtime_ms(&path) != entry.mtime {
        return None;
    }
    Some(path)
}

/// 记录一次生成结果。
fn packet_cache_store(book_root: &Path, key: String, digest: String, packet: &Path) {
    let mut cache = read_packet_cache(book_root);
    cache.entries.insert(
        key,
        PacketCacheEntry {
            path: packet.display().to_string(),
            mtime: mtime_ms(packet),
            sources: digest,
            at: now_ms(),
        },
    );
    let mut doomed: Vec<String> = Vec::new();
    if cache.entries.len() > PACKET_CACHE_MAX {
        let mut by_age: Vec<(String, u64)> = cache
            .entries
            .iter()
            .map(|(k, v)| (k.clone(), v.at))
            .collect();
        by_age.sort_by_key(|(_, at)| *at);
        let drop_count = cache.entries.len() - PACKET_CACHE_MAX;
        for (k, _) in by_age.into_iter().take(drop_count) {
            if let Some(gone) = cache.entries.remove(&k) {
                doomed.push(gone.path);
            }
        }
    }
    write_packet_cache(book_root, &cache);
    // 索引写完再动磁盘：索引里已经没有这些条目了，删文件不会让「有索引没文件」，
    // 万一删失败也只是留下一个孤儿文件，下次 sweep 还会再试一次（幂等）。
    for path in doomed {
        remove_packet_file(book_root, Path::new(&path));
    }
    sweep_orphan_packets(book_root, &cache);
}

/// 文件名是不是本程序生成的任务包（`outline-001.md` → 是，`我的细纲-最终版.md` → 不是）。
///
/// 只认脚本写死的格式 `<字母前缀>-<恰好 3 位数字>.md`（见 `build_architect_brief.py` 的
/// `out = out_dir / f"{args.task}-{number:03d}.md"`）。手写大纲、用户导出的提示词
/// 永远匹配不上，清理时一个都不会碰。
fn is_generated_packet_name(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    let Some((prefix, digits)) = stem.rsplit_once('-') else {
        return false;
    };
    !prefix.is_empty()
        && prefix.chars().all(|c| c.is_ascii_alphabetic())
        && digits.len() == 3
        && digits.chars().all(|c| c.is_ascii_digit())
}

/// 校验「这个路径确实是本程序写在 `<书>/.storydesk/prompts/` 下的任务包」。
///
/// `canonicalize` 顺手把 `..` 与软链接解开后再比父目录：缓存索引里被塞进
/// `../../正文/第001章.md` 这种路径时，`parent()` 不会是缓存目录，直接否掉。
fn is_packet_in_cache_dir(book_root: &Path, path: &Path) -> bool {
    let Ok(dir) = book_root.join(PACKET_DIR_REL).canonicalize() else {
        return false;
    };
    let Ok(real) = path.canonicalize() else {
        return false;
    };
    if real.parent() != Some(dir.as_path()) {
        return false;
    }
    real.file_name()
        .and_then(|name| name.to_str())
        .map(is_generated_packet_name)
        .unwrap_or(false)
}

/// 索引里还活着的任务包路径（统一成绝对路径，方便比较）。
fn live_packet_paths(cache: &PacketCache) -> Vec<PathBuf> {
    cache
        .entries
        .values()
        .map(|entry| {
            let path = PathBuf::from(&entry.path);
            path.canonicalize().unwrap_or(path)
        })
        .collect()
}

/// 删掉一份任务包文件；不属于本程序生成物的路径一律不动。
fn remove_packet_file(book_root: &Path, path: &Path) {
    if !is_packet_in_cache_dir(book_root, path) {
        return;
    }
    match std::fs::remove_file(path) {
        Ok(()) => eprintln!("[storydesk] 任务包缓存已淘汰：{}", path.display()),
        Err(e) => eprintln!(
            "[storydesk] 任务包缓存淘汰失败（不影响本轮执行）：{}：{e}",
            path.display()
        ),
    }
}

/// 清掉缓存目录里「本程序生成、但索引里已经没有」的任务包。
///
/// `PACKET_CACHE_MAX` 以前只管索引条目、从不删文件：实测 `.storydesk/prompts/`
/// 积到 63 个 `outline-*.md`，索引里只剩 1 条。旧包除了占盘没有用处，更麻烦的是
/// 同名文件（`outline-001.md`）新旧混在一起，排查时无法分辨哪份是这一轮写的。
///
/// 三重保险，宁可不删也不删错：只删缓存目录下、文件名符合脚本生成格式、且
/// 索引里没人引用的；再要求 30 分钟没被碰过（避免删掉正在生成/正在读取的包）。
fn sweep_orphan_packets(book_root: &Path, cache: &PacketCache) {
    let dir = book_root.join(PACKET_DIR_REL);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let live = live_packet_paths(cache);
    let cutoff = now_ms().saturating_sub(ORPHAN_GRACE_MS);
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if !is_generated_packet_name(&name) {
            continue;
        }
        let real = path.canonicalize().unwrap_or_else(|_| path.clone());
        if live.iter().any(|kept| kept == &real) {
            continue;
        }
        let touched = mtime_ms(&path);
        if touched == 0 || touched > cutoff {
            continue;
        }
        remove_packet_file(book_root, &path);
    }
}

/// `build_architect_brief.py --project <书> --task world|volume|outline [...]`
///
/// 脚本把任务包写到 `.storydesk/prompts/`（或 `.story/` 下）并打印路径；这里从输出里认路径。
///
/// R3 项 6 第一条：同一 `task + volume + chapters`、且 `设定/追踪/大纲/参考资料/对标` 的
/// mtime 都没变时，直接复用上一份任务包——不启动 Python、不重写文件。
pub fn build_task_packet(
    workspace_root: &Path,
    book_root: &Path,
    task: &str,
    volume: Option<u32>,
    chapters: Option<(u32, u32)>,
) -> Result<TaskPacket, StoryError> {
    let key = packet_cache_key(task, volume, chapters);
    let digest = packet_sources_digest(book_root);
    if let Some(path) = packet_cache_lookup(book_root, &key, &digest) {
        eprintln!(
            "[storydesk] 任务包缓存命中（跳过 Python）：{}",
            path.display()
        );
        return Ok(TaskPacket {
            path: path.display().to_string(),
            task: task.to_string(),
            raw: format!("（任务包缓存命中：{}）", path.display()),
        });
    }

    let scripts = resolve(workspace_root);
    let Some(brief) = scripts.brief.clone() else {
        return Err(StoryError::env_missing(
            "build_architect_brief",
            "未找到 build_architect_brief.py，无法生成任务包。",
            "在项目根运行 /story-setup（Codex 用 $story-setup）后再试。",
        ));
    };

    let mut args = vec![
        "--project".to_string(),
        book_root.display().to_string(),
        "--task".to_string(),
        task.to_string(),
    ];
    if let Some(v) = volume {
        args.push("--volume".to_string());
        args.push(v.to_string());
    }
    if let Some((from, to)) = chapters {
        args.push("--chapters".to_string());
        args.push(format!("{from}-{to}"));
    }

    let out = run_python(&scripts, &brief, &args, Some(workspace_root))?;
    if !out.ok {
        return Err(StoryError::invalid(
            "taskPacket",
            format!(
                "生成任务包失败（exit {}）：{}",
                out.code,
                out.stderr.trim().replace('\n', " ")
            ),
        ));
    }

    // 脚本输出可能是 JSON，也可能是「任务包：<路径>」；两种都认。
    let raw = format!("{}\n{}", out.stdout, out.stderr);
    let path = extract_md_path(&raw).ok_or_else(|| {
        StoryError::internal(format!(
            "任务包脚本已执行但没有返回 .md 路径，原始输出：{}",
            raw.trim()
        ))
    })?;

    // 记进缓存：下一次同 task/volume/chapters 且源文件没变时不再起 Python。
    packet_cache_store(book_root, key, digest, Path::new(&path));

    Ok(TaskPacket {
        path,
        task: task.to_string(),
        raw: out.stdout,
    })
}

/// 从脚本输出里挑出任务包 .md 路径（取最后出现的那个，脚本会把关键路径打在末尾）。
fn extract_md_path(raw: &str) -> Option<String> {
    let mut found: Option<String> = None;
    // 阶段 11.6：也按空白切分——实测脚本会打出「已写入 追踪/伏笔.md 与 完成」
    // 这种内联路径；带空格的正式路径脚本侧都会加引号，引号在这里仍然是切分边界。
    for token in raw.split(|c: char| c == '"' || c == '\'' || c == ',' || c.is_whitespace()) {
        let mut t = token.trim().trim_end_matches('。').trim();
        if !t.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        // 「任务包：D:\…」这种带标签的，取全角冒号后面的纯路径
        //（Windows 盘符用的是半角冒号，不会误伤）。
        if let Some(i) = t.rfind('：') {
            t = t[i + '：'.len_utf8()..].trim();
        }
        let looks_like_path = t.contains(":\\")
            || t.contains(":/")
            || t.starts_with('/')
            || t.contains(".story")
            || t.contains(".storydesk")
            || t.contains('/');
        if looks_like_path && !t.is_empty() {
            found = Some(t.to_string());
        }
    }
    found
}

// ---------------------------------------------------------------- 阶段 5：写后体检

/// 正文最少字数（与 storyctl 的默认 `--min-chars` 同量级；这里是本地兜底口径）。
pub const PROSE_MIN_CHARS: usize = 800;

/// 正文里算「字数」的字符：只数文字与数字，空白、标点与 Markdown 标记都不计。
///
/// 与 `services::context::prose_char_count` 共用同一口径：模型自查「去标点字数」，
/// 应用验收也必须去标点，否则两边永远差几百字。
fn count_prose_chars(body: &str) -> usize {
    crate::services::context::prose_char_count(body)
}

/// 找某一章的正文文件（`正文/第NNN章_*.md`，兼容 `第N章`）。
///
/// 按解析出的章号比较，不能用 `starts_with("第2章")`——那会把「第20章」也算成第 2 章。
fn find_prose_files(book_root: &Path, chapter: u32) -> Vec<PathBuf> {
    let dir = book_root.join("正文");
    let mut hits = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !name.ends_with(".md") {
                continue;
            }
            if crate::services::context::chapter_in_name(name) == Some(chapter) {
                hits.push(path);
            }
        }
    }
    hits
}

/// 写后体检里的事件一致性：一章盖着几张事件卡，就逐张查。
///
/// 早期实现只认细纲里那一张「所属事件」：一章同时挂着两张卡时，第二张卡的消耗项、
/// 锚词、自造编号全都不体检——而多卡并行正是跨章展开的主用法，漏掉它等于漏掉一半。
fn continuity_issues(book_root: &Path, chapter: u32, body: &str) -> Vec<PostCheckIssue> {
    let declared = crate::services::context::declared_event(book_root, chapter);
    let cards = crate::services::context::events_covering(book_root, chapter);
    let mut out: Vec<PostCheckIssue> = Vec::new();
    let issue = |event: Option<&str>, text: String| PostCheckIssue {
        code: "continuity".to_string(),
        text,
        event: event.map(str::to_string),
    };

    if let Some(id) = declared.as_deref() {
        if cards.is_empty() {
            out.push(issue(
                Some(id),
                format!(
                    "细纲声称属于事件 {id}，但 大纲/事件/ 下没有这张卡——这一章等于没有归属。\
                     补卡，或者把细纲改成「所属事件：无」。"
                ),
            ));
            return out;
        }
    }
    if cards.is_empty() {
        return out;
    }

    // ① 每张卡的「本章消耗」都要在正文里见到锚词。
    for card in &cards {
        let label = format!("事件 {}（{}）", card.id, card.title);
        let Some(role) = card.role_of(chapter) else {
            // 「没排章型」只对细纲点名的那张卡算问题；区间覆盖顺带盖到的第二章不受这条约束。
            if declared.as_deref() == Some(card.id.as_str()) {
                out.push(issue(
                    Some(card.id.as_str()),
                    format!(
                        "{label} 的章型分工里没有第 {chapter} 章，这一章在事件里没有位置（区间覆盖所以放行）。"
                    ),
                ));
            }
            continue;
        };
        for code in &role.consumes {
            let Some(item) = card.open_items.iter().find(|item| &item.code == code) else {
                out.push(issue(
                    Some(card.id.as_str()),
                    format!(
                        "{label} 的章型分工说本章消耗 {code}，但卡里的「未结项」没有这一项（可能已经结算）。\
                         要么改分工，要么把它写回未结项。"
                    ),
                ));
                continue;
            };
            if item.anchors.is_empty() {
                continue;
            }
            if !item
                .anchors
                .iter()
                .any(|anchor| body.contains(anchor.as_str()))
            {
                out.push(issue(
                    Some(card.id.as_str()),
                    format!(
                        "{label} 承诺本章消耗 {code}（锚词：{}），正文里一个锚词都没出现——\
                         这一项等于没结算，别只是让谁提了一句就过。",
                        item.anchors.join("、")
                    ),
                ));
            }
        }
    }

    // ② 上游遗留：前置卡还有没结的项，本卡既没登记、正文也没交代。
    let mut reported: Vec<(String, String)> = Vec::new();
    for card in &cards {
        let Some(prev) = card.prev.as_deref() else { continue };
        if prev == card.id || cards.iter().any(|other| other.id == prev) {
            // 前置卡自己也盖着本章：它那一段已经查过，别当「上游」再报一遍。
            continue;
        }
        let Some(upstream) = crate::services::context::find_event_card(book_root, prev) else {
            out.push(issue(
                Some(prev),
                format!(
                    "事件 {} 指着前置事件 {prev}，但 大纲/事件/ 下没有这张卡：前史无处可查，动笔前先确认事件号。",
                    card.id
                ),
            ));
            continue;
        };
        for item in upstream.open_at(chapter) {
            if card.open_items.iter().any(|mine| mine.code == item.code) {
                continue;
            }
            if reported
                .iter()
                .any(|(event, code)| event == &upstream.id && code == &item.code)
            {
                continue;
            }
            reported.push((upstream.id.clone(), item.code.clone()));
            let mut text = format!(
                "上游事件 {} · {} 的 {} 还没结",
                upstream.id, upstream.title, item.code
            );
            if !item.text.trim().is_empty() {
                text.push_str(&format!("（{}）", item.text.trim()));
            }
            text.push_str(&format!(
                "，本章（事件 {}）既没在正文里交代、也没登记进未结项——跨事件承接断在这里。",
                card.id
            ));
            out.push(issue(Some(upstream.id.as_str()), text));
        }
    }

    // ③ 正文冒出所有卡都没有的同前缀编号 = 写手自己开了新账（正是「每章自造钩子」的病）。
    //    只查同一前缀（U/F/H/P/B/R），免得把 AK-47、F-16 这类当真。
    let prefix = |code: &str| code.chars().next();
    let mut known_prefixes: Vec<char> = Vec::new();
    for card in &cards {
        known_prefixes.extend(card.open_items.iter().filter_map(|item| prefix(&item.code)));
        known_prefixes.extend(card.ledger.iter().filter_map(|item| prefix(&item.code)));
    }
    let primary = declared.clone().unwrap_or_else(|| cards[0].id.clone());
    for token in codes_in(body) {
        let Some(head) = prefix(&token) else { continue };
        if !known_prefixes.contains(&head) {
            continue;
        }
        let known = cards.iter().any(|card| {
            card.open_items.iter().any(|item| item.code == token) || card.is_settled(&token)
        });
        if !known {
            out.push(issue(
                Some(primary.as_str()),
                format!(
                    "正文里出现编号 {token}，本章覆盖的事件卡（{}）里都没有它——\
                     新开的事必须写进卡里，否则下一章没人接。",
                    cards
                        .iter()
                        .map(|card| card.id.as_str())
                        .collect::<Vec<_>>()
                        .join("、")
                ),
            ));
        }
    }

    out
}

/// 正文里形如 `U-001` / `F-12` 的编号（去重、保序）。
fn codes_in(body: &str) -> Vec<String> {
    let chars: Vec<char> = body.chars().collect();
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i + 2 < chars.len() {
        let hit = chars[i].is_ascii_uppercase() && chars[i + 1] == '-' && chars[i + 2].is_ascii_digit();
        if !hit {
            i += 1;
            continue;
        }
        let mut j = i + 2;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        let code: String = chars[i..j].iter().collect();
        if !out.contains(&code) {
            out.push(code);
        }
        i = j;
    }
    out
}

/// 写完正文后的体检：本地字数口径 + （装了脚本时）`storyctl.py chapter check`。
///
/// 这里**永不返回 Err**：体检失败本身就是一条结果，不能把整条流水线打成「出错」。
pub fn postcheck_prose(
    workspace_root: &Path,
    book_root: &Path,
    chapter: u32,
    run_id: &str,
) -> PostCheck {
    let mut issues: Vec<PostCheckIssue> = Vec::new();
    let mut stdout = String::new();
    let mut stderr = String::new();
    let mut chars: Option<usize> = None;
    let mut file: Option<String> = None;

    let hits = find_prose_files(book_root, chapter);
    let mut local_ok = true;
    // 达标线：细纲写了「正文目标字数」就按它的下限来，没写才退回 800 的兜底口径。
    // 事故：细纲要 6200 字，写手交 5389 字，旧实现只比 800 就报「通过」。
    let outline_target = crate::services::context::outline_word_range(book_root, chapter);
    let floor = outline_target
        .map(|(lo, _)| lo as usize)
        .unwrap_or(PROSE_MIN_CHARS);

    match hits.len() {
        0 => {
            local_ok = false;
            issues.push(PostCheckIssue {
                event: None,
                code: "missing".to_string(),
                text: format!("没有找到第 {chapter} 章的正文文件（正文/第{chapter:03}章_*.md）"),
            });
        }
        1 => {
            let path = &hits[0];
            let rel = path
                .strip_prefix(book_root)
                .map(|p| p.display().to_string().replace('\\', "/"))
                .unwrap_or_else(|_| path.display().to_string());
            file = Some(rel);
            // 读不出来（被占用 / 非 UTF-8）不能当空文件：否则会按 0 字判「字数不足」，
            // 上层还可能据此自动续写。只有读成功才做编码与字数判定。
            let read = crate::services::atomic_write::read_text(path);
            let body = read.as_deref().unwrap_or_default();
            if let Err(e) = &read {
                local_ok = false;
                issues.push(PostCheckIssue {
                    event: None,
                    code: "unreadable".to_string(),
                    text: format!(
                        "第 {chapter} 章正文读不出来（{e}），已跳过编码与字数判定，不会自动续写。"
                    ),
                });
            } else if crate::services::encoding_guard::looks_like_mojibake(body) {
                // 编码坏了就不报字数：乱码里「字数」是假的（cp936 解码会把 3 个字节变成 2 个字），
                // 报出来只会让作者以为是「这章写短了」，而不是「文件被写坏了」。
                local_ok = false;
                let head = crate::services::encoding_guard::sample(body, 48);
                issues.push(PostCheckIssue {
                    event: None,
                    code: "encoding".to_string(),
                    text: format!(
                        "第 {chapter} 章正文疑似编码损坏（UTF-8 被按 GBK 读写过）：`{head}`。\
                         这一章的字符已经不可逆，请用 Write 工具重写，或从 .storydesk/snapshots 恢复；\
                         改稿不要用 PowerShell 的 Get-Content / Set-Content。"
                    ),
                });
            } else {
                let n = count_prose_chars(body);
                chars = Some(n);
                if n < floor {
                    local_ok = false;
                    let text = match outline_target {
                        Some((lo, hi)) => {
                            format!("字数不足：{n} 字（本章细纲要求 {lo}-{hi} 字，不计标点与空白）")
                        }
                        None => format!("字数不足：{n} 字（建议 ≥ {floor} 字）"),
                    };
                    issues.push(PostCheckIssue {
                        event: None,
                        code: "wordcount".to_string(),
                        text,
                    });
                }
                // 字数之外再看事件一致性：承诺消耗的未结项到底有没有落到正文里。
                // 编码损坏时跳过——乱码里匹配锚词没有意义。
                issues.extend(continuity_issues(book_root, chapter, body));
            }
        }
        n => {
            local_ok = false;
            issues.push(PostCheckIssue {
                event: None,
                code: "structure".to_string(),
                text: format!("第 {chapter} 章有 {n} 份正文文件，storyctl 要求恰好一份"),
            });
        }
    }

    // 装了脚本就再跑一次真体检（AI 味 / 结构 / 追踪一致性都在里面）。
    let scripts = resolve(workspace_root);
    if scripts.gate_ready() {
        match chapter_check(workspace_root, book_root, chapter) {
            Ok(out) => {
                stdout = out.stdout.clone();
                stderr = out.stderr.clone();
                if !out.ok {
                    let text = out
                        .stdout
                        .lines()
                        .chain(out.stderr.lines())
                        .map(str::trim)
                        .find(|l| !l.is_empty())
                        .unwrap_or("storyctl 体检未通过")
                        .to_string();
                    let code =
                        if text.contains("句式") || text.contains("AI 味") || text.contains("欠账")
                        {
                            "ai_pattern"
                        } else if text.contains("字数") {
                            "wordcount"
                        } else {
                            "script"
                        };
                    // 同一类问题别重复报两遍（本地字数已经报过就只留脚本原话在 stdout 里）。
                    if !(code == "wordcount" && issues.iter().any(|i| i.code == "wordcount")) {
                        issues.push(PostCheckIssue {
                            event: None,
                            code: code.to_string(),
                            text,
                        });
                    }
                }
            }
            Err(e) => {
                stderr = e.to_string();
                issues.push(PostCheckIssue {
                    event: None,
                    code: "script".to_string(),
                    text: format!("storyctl 体检没跑成：{e}"),
                });
            }
        }
    } else if hits.len() == 1 {
        // 没装脚本时只做本地字数口径，别把「没脚本」当成体检失败。
        stdout = match outline_target {
            Some((lo, hi)) => {
                format!("未检测到 storyctl.py，本次只做本地字数体检（本章细纲要求 {lo}-{hi} 字）。")
            }
            None => format!("未检测到 storyctl.py，本次只做本地字数体检（阈值 {floor} 字）。"),
        };
    }

    let ok = local_ok && issues.is_empty();

    let reason = if ok {
        match (chars, outline_target) {
            (Some(n), Some((lo, hi))) => format!("体检通过：{n} 字（本章细纲 {lo}-{hi} 字）"),
            (Some(n), None) => format!("体检通过：{n} 字，未发现 AI 味与结构问题"),
            (None, _) => "体检通过".to_string(),
        }
    } else if let Some(first) = issues.first() {
        format!("体检未通过：{}", first.text)
    } else {
        "体检未通过".to_string()
    };

    PostCheck {
        run_id: run_id.to_string(),
        chapter,
        ok,
        skipped: false,
        reason,
        issues,
        chars,
        file,
        stdout,
        stderr,
        checked_at: crate::services::snapshot::iso_now(),
    }
}

#[cfg(test)]
mod tests {
    fn out_with(stdout: &str) -> GateOutput {
        GateOutput {
            ok: false,
            code: 2,
            stdout: stdout.to_string(),
            stderr: String::new(),
        }
    }

    #[test]
    fn missing_outline_is_repairable_by_writing_it() {
        let out = out_with("这次没查成：第 331 章没有细纲——大纲/细纲_第331章.md 不存在，先写这一章细纲再写正文。");
        assert_eq!(
            classify_block(331, &out),
            (GateVerdict::Repairable, GateRemedy::WriteOutline(331))
        );
    }

    #[test]
    fn thin_outline_is_repairable() {
        let out = out_with("这次没查成：第 12 章的细纲是空的（不计 # 号和空白不到 200 字）。");
        assert_eq!(
            classify_block(12, &out),
            (GateVerdict::Repairable, GateRemedy::WriteOutline(12))
        );
    }

    #[test]
    fn duplicate_outlines_are_not_auto_repaired() {
        // 多份细纲是人工整理问题：自动补一步只会再造一份，越补越乱，必须停下。
        let out = out_with("这次没查成：第 7 章细纲不止一份（细纲_第7章.md、细纲_第007章_旧.md），且没有一份叫 细纲_第007章.md。先删到只剩一份再写正文。");
        assert_eq!(
            classify_block(7, &out),
            (GateVerdict::Stop, GateRemedy::None)
        );
    }

    #[test]
    fn outline_read_failure_is_not_auto_repaired() {
        // 读不出来 ≠ 没有细纲：这条文案过去可能因为错误串里带「不存在」被判成可修复，
        // 于是自动补写一份，把作者的原稿顶掉。
        let out = out_with(
            "这次没查成：第 9 章的细纲文件读不出来（系统找不到指定的文件。不存在）。已停止，不自动重写细纲。",
        );
        assert_eq!(
            classify_block(9, &out),
            (GateVerdict::Stop, GateRemedy::None)
        );
    }

    /// 合并卷里已经有这一章 → gate 不能再判「没有细纲」并自动补写第二份细纲。
    #[test]
    fn merged_volume_outline_is_not_missing() {
        let root = std::env::temp_dir().join(format!(
            "storydesk-gate-merged-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        let body = "陈砚在仓库对账，老赵递来饭票，两人把数目核到卯时。".repeat(30);
        std::fs::write(
            root.join("大纲").join("卷1_第001-010章.md"),
            format!(
                "# 卷1\n\n## 第1章 开门\n开篇。\n\n## 第8章 教不是代\n{body}\n\n## 第9章 卯时\n收束。\n"
            ),
        )
        .unwrap();

        let out = local_outline_check(&root, 8);
        assert!(
            out.ok,
            "合并卷里已有第 8 章，gate 不该判没有细纲：{}",
            out.stdout
        );
        assert_ne!(
            classify_block(8, &out),
            (GateVerdict::Repairable, GateRemedy::WriteOutline(8))
        );

        // 真的没有这一章时，仍然走原来的「缺细纲」路径。
        let gap = local_outline_check(&root, 5);
        assert!(!gap.ok, "第 5 章确实没有细纲");
        assert_eq!(
            classify_block(5, &gap),
            (GateVerdict::Repairable, GateRemedy::WriteOutline(5))
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn event_card_mismatch_is_repairable() {
        let out = out_with("这次没查成：第 40 章的事件卡已结算（事件 401），不能再往里排章。");
        assert_eq!(
            classify_block(40, &out),
            (GateVerdict::Repairable, GateRemedy::FixEventCard(40))
        );
    }

    #[test]
    fn unknown_hook_message_stays_a_stop() {
        // 外部 hook 脚本的未知文案不能自动放行：看不懂的拦截就停下让人看。
        let out = GateOutput {
            ok: false,
            code: 3,
            stdout: String::new(),
            stderr: "storyctl: unexpected token at line 3".to_string(),
        };
        assert_eq!(
            classify_block(5, &out),
            (GateVerdict::Stop, GateRemedy::None)
        );
        assert_eq!(
            classify_block(5, &GateOutput { ok: true, ..out }),
            (GateVerdict::Pass, GateRemedy::None)
        );
    }

    use super::*;
    use crate::models::dto::HookBlockKind;
    use std::fs;

    #[test]
    fn picks_last_md_path() {
        let raw = "正在生成…\n任务包：D:\\novel\\长篇\\我的小说\\.storydesk\\prompts\\outline-001.md\n完成";
        assert_eq!(
            extract_md_path(raw).unwrap(),
            "D:\\novel\\长篇\\我的小说\\.storydesk\\prompts\\outline-001.md"
        );
    }

    #[test]
    fn ignores_plain_words() {
        assert!(extract_md_path("已写入 追踪/伏笔.md 与 完成").is_some());
        assert!(extract_md_path("nothing here").is_none());
    }

    #[test]
    fn counts_prose_without_markup() {
        assert_eq!(count_prose_chars("# 标题\n\n正文"), 2); // 标题行不计
        assert_eq!(count_prose_chars("> 引用\n- 列表"), 2); // 引用行不计
    }

    // ------------------------------------------------------------ 事件层

    const EVENT_CARD: &str = "# 事件 002 · 码头交接

## 基本信息

| 所属卷 | 章号区间 | 前置事件 | 状态 |
| --- | --- | --- | --- |
| 1 | 2-5 | — | active |

## 未结项

| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |
| --- | --- | --- | --- | --- |
| U-001 | 仓单第三联不在 | 2 | 3 | 第三联、复印件 |
| U-002 | 老周改口 | 2 | 4 | 改口、签字 |

## 章型分工

| 章 | 章型 | 本章消耗 | 本章新增 |
| --- | --- | --- | --- |
| 3 | 推 | U-001 | — |
| 4 | 转 | U-002 | — |
";

    fn event_book(name: &str) -> PathBuf {
        let root = cache_book(name);
        fs::create_dir_all(root.join("大纲").join("事件")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        root
    }

    /// 写一份够 300 字的细纲：字数门槛是前置门禁的第一关，事件检查只在它过了之后才跑。
    fn write_outline(root: &Path, chapter: u32, declared: &str) {
        let filler = "本章围绕仓单与对账展开，人物在码头逐项核对，冲突一层层收紧。".repeat(12);
        let body = format!(
            "# 第 {chapter} 章\n\n所属事件：{declared}\n\n## 本章目标\n\n{filler}\n"
        );
        fs::write(root.join(format!("大纲/细纲_第{chapter:03}章.md")), body).unwrap();
    }

    #[test]
    fn precheck_lets_a_book_without_an_event_layer_through() {
        // 真实事故：《隐藏条款》第 301 章细纲写着「所属事件：事件 301」，全书却没有 大纲/事件/。
        // 旧逻辑在这里直接拦，于是第一章之后整批停摆，330 章的细纲永远排不到。
        let root = cache_book("ev-nocards");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        write_outline(&root, 3, "事件 301 · 别签这一份");
        assert!(!root.join("大纲/事件").exists(), "这本书本来就没有事件层");

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "没有事件层的书不该被事件检查拦：{}", out.stdout);
        assert_eq!(out.code, 0);
        assert!(out.stdout.contains("事件层提示"), "{}", out.stdout);
        assert!(out.stdout.contains("事件 301"), "{}", out.stdout);
        assert!(out.stdout.contains("本书还没有任何事件卡"), "{}", out.stdout);
        let (kind, _) = crate::services::hook_parse::classify(&out.stdout);
        assert!(!matches!(kind, HookBlockKind::EventMissing { .. }), "{kind:?}");
    }

    #[test]
    fn precheck_lets_an_invented_event_number_through_with_a_note() {
        // 有事件层、但模型照自己的编号习惯写了「事件 301」——同样只提示，因为这一行字段
        // 只是一句声明，拦下去把整条流水停住，收益（少记一次账）远小于代价。
        let root = event_book("ev-madeup");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 3, "事件 301 · 别签这一份");

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "编出来的事件号不该拦：{}", out.stdout);
        assert!(out.stdout.contains("事件层提示"), "{}", out.stdout);
        assert!(out.stdout.contains("大纲/事件/ 下没有这张卡"), "{}", out.stdout);
        let (kind, _) = crate::services::hook_parse::classify(&out.stdout);
        assert!(!matches!(kind, HookBlockKind::EventMissing { .. }), "{kind:?}");
    }

    #[test]
    fn outline_check_names_the_file_that_is_missing() {
        let root = cache_book("ol-missing");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(!out.ok);
        assert!(out.stdout.contains("第 3 章没有细纲"), "{}", out.stdout);
        assert!(out.stdout.contains("大纲/细纲_第003章.md 不存在"), "{}", out.stdout);
        assert!(!out.stdout.contains("细纲不止一份"), "缺就是缺，别把两种情况混成一句：{}", out.stdout);
    }

    #[test]
    fn outline_check_prefers_the_canonical_name_over_copies() {
        // 历史副本（细纲_第3章.md、细纲_第003章_旧.md）不该让这一章卡死。
        let root = cache_book("ol-dupes");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        write_outline(&root, 3, "无");
        fs::write(root.join("大纲/细纲_第3章.md"), "# 第 3 章

旧版
").unwrap();
        fs::write(root.join("大纲/细纲_第003章_旧.md"), "# 第 3 章

更旧
").unwrap();

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "认了标准命名就该放行：{}", out.stdout);
        assert!(out.stdout.contains("忽略了同名副本"), "{}", out.stdout);
        let (kind, _) = crate::services::hook_parse::classify(&out.stdout);
        assert!(!matches!(kind, HookBlockKind::MissingOutline { .. }), "{kind:?}");
    }

    #[test]
    fn outline_check_reports_the_names_when_none_is_canonical() {
        let root = cache_book("ol-nocanon");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(root.join("大纲/细纲_第03章.md"), "# 第 3 章

甲
").unwrap();
        fs::write(root.join("大纲/细纲_第3章_副本.md"), "# 第 3 章

乙
").unwrap();

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(!out.ok, "两份都不是标准命名，得让人自己挑：{}", out.stdout);
        assert!(out.stdout.contains("第 3 章细纲不止一份（"), "{}", out.stdout);
        assert!(out.stdout.contains("细纲_第03章.md"), "{}", out.stdout);
        assert!(out.stdout.contains("细纲_第3章_副本.md"), "{}", out.stdout);
    }

    #[test]
    fn event_precheck_does_not_depend_on_the_chapter_number() {
        // 事故只在 301 章暴露，但判断里没有任何章号假设：任何一章、任何一本书走同一条路。
        for chapter in [1u32, 7, 42, 301, 320, 330, 999] {
            let root = cache_book(&format!("ev-any-{chapter}"));
            fs::create_dir_all(root.join("大纲")).unwrap();
            fs::create_dir_all(root.join("正文")).unwrap();
            write_outline(&root, chapter, &format!("事件 {chapter} · 自编号"));

            let out = precheck_prose(&root, &root, chapter).unwrap();
            assert!(out.ok, "第 {chapter} 章不该被拦：{}", out.stdout);
            assert!(out.stdout.contains("事件层提示"), "第 {chapter} 章：{}", out.stdout);
        }
    }

    #[test]
    fn a_real_card_still_blocks_outside_its_own_range() {
        // 区别在「卡存不存在」，不在章号：这张卡（章号区间 2-5、分工 3/4）在第 5 章放行，第 6 章起与 321 章照样拦。
        let root = event_book("ev-span");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();

        for chapter in [3u32, 4, 5] {
            write_outline(&root, chapter, "事件 002 · 码头交接");
            let out = precheck_prose(&root, &root, chapter).unwrap();
            assert!(out.ok, "第 {chapter} 章在卡的区间/分工里：{}", out.stdout);
        }
        for chapter in [6u32, 321, 330] {
            write_outline(&root, chapter, "事件 002 · 码头交接");
            let out = precheck_prose(&root, &root, chapter).unwrap();
            assert!(!out.ok, "第 {chapter} 章不在分工里，仍要拦：{}", out.stdout);
            assert!(out.stdout.contains("章型分工"), "{}", out.stdout);
        }
    }

    #[test]
    fn a_hand_written_card_still_gates_and_feeds_the_digest() {
        const HAND: &str = "# 事件 002 · 手写卡\n\n## 基本信息\n\n- 事件号：002\n- 章号区间：2—5\n- 状态：进行中\n\n## 完成条件\n\n1. 对完账\n\n## 未结项\n\n- U-001 仓单第三联不在谁手里（埋于 2，最迟 4，锚词：第三联）\n\n## 章型分工\n\n- 2 起\n- 3 推（消耗 U-001）\n- 4 转\n- 5 结\n";
        let root = event_book("ev-hand");
        // 文件名不带事件号，号写在正文里：手写卡就是这种起名法。
        fs::write(root.join("大纲/事件/手写卡.md"), HAND).unwrap();

        write_outline(&root, 3, "事件 002 · 手写卡");
        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "第 3 章排了章型分工，该放行：{}", out.stdout);

        // 注入走的是 event_digest，不在门禁的 stdout 里：单独验一遍手写卡能不能供上。
        let digest = crate::services::context::event_digest(&root, 3).expect("第 3 章在事件里");
        assert!(digest.contains("手写卡"), "注入里要有事件标题：{digest}");
        assert!(digest.contains("U-001"), "未结项要随注入下发：{digest}");

        write_outline(&root, 9, "事件 002 · 手写卡");
        let blocked = precheck_prose(&root, &root, 9).unwrap();
        assert!(!blocked.ok, "第 9 章不在 2—5 里，要拦：{}", blocked.stdout);
    }

    #[test]
    fn continuity_checks_every_card_covering_the_chapter() {
        const SECOND: &str = "# 事件 003 · 暗账\n\n## 基本信息\n\n| 所属卷 | 章号区间 | 前置事件 | 状态 |\n| --- | --- | --- | --- |\n| 1 | 2-5 | — | active |\n\n## 未结项\n\n| 编号 | 内容 | 埋于 | 最迟结算章 | 锚词 |\n| --- | --- | --- | --- | --- |\n| U-101 | 暗账本对不上 | 2 | 4 | 暗账本、红笔 |\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 3 | 推 | U-101 | — |\n";
        let root = event_book("ev-two-cards");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        fs::write(root.join("大纲/事件/事件_003_暗账.md"), SECOND).unwrap();
        // 细纲只点了 002：另一张卡靠区间/分工盖住本章，也必须一起体检。
        write_outline(&root, 3, "事件 002 · 码头交接");

        let only_one = continuity_issues(&root, 3, "老周把复印件拍在桌上，天色渐暗。");
        assert!(
            only_one
                .iter()
                .any(|issue| issue.event.as_deref() == Some("003")),
            "第二张卡的锚词没落地，要单独报出来：{only_one:?}"
        );
        assert!(
            !only_one
                .iter()
                .any(|issue| issue.event.as_deref() == Some("002")),
            "002 的锚词已经在正文里，不该连坐：{only_one:?}"
        );

        let both = continuity_issues(&root, 3, "老周把复印件拍在桌上，那本暗账本压在茶杯底下。");
        assert!(both.is_empty(), "两张卡的锚词都写了，不该有问题：{both:?}");
    }

    #[test]
    fn continuity_names_the_upstream_event_it_was_handed() {
        const DOWNSTREAM: &str = "# 事件 004 · 交接之后\n\n## 基本信息\n\n| 所属卷 | 章号区间 | 前置事件 | 状态 |\n| --- | --- | --- | --- |\n| 1 | 8-10 | 002 | active |\n\n## 章型分工\n\n| 章 | 章型 | 本章消耗 | 本章新增 |\n| --- | --- | --- | --- |\n| 8 | 起 | — | — |\n";
        let root = event_book("ev-upstream-gate");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        fs::write(root.join("大纲/事件/事件_004_交接之后.md"), DOWNSTREAM).unwrap();
        write_outline(&root, 8, "事件 004 · 交接之后");

        let issues = continuity_issues(&root, 8, "他回到码头，天已经黑了。");
        let upstream: Vec<&crate::models::dto::PostCheckIssue> = issues
            .iter()
            .filter(|issue| issue.event.as_deref() == Some("002"))
            .collect();
        assert!(
            !upstream.is_empty(),
            "前置事件还有没结的项，要按 002 报出来：{issues:?}"
        );
        assert!(
            upstream
                .iter()
                .any(|issue| issue.text.contains("上游事件 002")),
            "{issues:?}"
        );
    }


    #[test]
    fn precheck_blocks_a_settled_event() {
        let root = event_book("ev-settled");
        let card = EVENT_CARD.replace("| active |", "| settled |");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), card).unwrap();
        write_outline(&root, 3, "事件 002 · 码头交接");

        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(!out.ok);
        assert!(out.stdout.contains("已结算"), "{}", out.stdout);
        let (kind, _) = crate::services::hook_parse::classify(&out.stdout);
        assert!(matches!(kind, HookBlockKind::EventSettled { .. }), "{kind:?}");
    }

    #[test]
    fn precheck_blocks_a_chapter_the_card_never_planned() {
        let root = event_book("ev-unplanned");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 9, "事件 002 · 码头交接");

        let out = precheck_prose(&root, &root, 9).unwrap();
        assert!(!out.ok);
        assert!(out.stdout.contains("章型分工"), "{}", out.stdout);
        let (kind, _) = crate::services::hook_parse::classify(&out.stdout);
        assert!(matches!(kind, HookBlockKind::EventUnplanned { chapter: 9, .. }), "{kind:?}");
    }

    #[test]
    fn precheck_passes_when_the_card_covers_the_chapter() {
        let root = event_book("ev-ok");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 3, "事件 002 · 码头交接");
        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "卡在、章在、事件没结，就该放行：{}", out.stdout);
    }

    #[test]
    fn precheck_leaves_old_books_alone() {
        let root = event_book("ev-old");
        write_outline(&root, 3, "无");
        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(out.ok, "细纲写「无」不该被事件层拦：{}", out.stdout);
    }

    #[test]
    fn outline_problem_wins_over_the_event_problem() {
        let root = event_book("ev-order");
        // 细纲只有几十个字，事件卡也不存在——应该先报细纲，别让人两头读。
        fs::write(root.join("大纲/细纲_第003章.md"), "# 第 3 章\n\n所属事件：事件 002。\n").unwrap();
        let out = precheck_prose(&root, &root, 3).unwrap();
        assert!(!out.ok);
        assert!(out.stdout.contains("细纲"), "{}", out.stdout);
        assert!(!out.stdout.contains("事件"), "{}", out.stdout);
    }

    #[test]
    fn continuity_flags_consumed_items_whose_anchors_never_appear() {
        let root = event_book("ev-cont");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 3, "事件 002 · 码头交接");

        let issues = continuity_issues(&root, 3, "林昭和老周在码头对账，天色渐暗。");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert_eq!(issues[0].code, "continuity");
        assert!(issues[0].text.contains("U-001"), "{}", issues[0].text);
        assert!(issues[0].text.contains("第三联"), "{}", issues[0].text);

        let clean = continuity_issues(&root, 3, "老周把那张复印件拍在桌上，纸角卷起。");
        assert!(clean.is_empty(), "{clean:?}");
    }

    #[test]
    fn continuity_flags_a_code_the_card_never_registered() {
        let root = event_book("ev-newcode");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 3, "事件 002 · 码头交接");

        let issues = continuity_issues(&root, 3, "老周提到复印件，又掏出一张 U-009 的单据。");
        assert_eq!(issues.len(), 1, "{issues:?}");
        assert!(issues[0].text.contains("U-009"), "{}", issues[0].text);

        // 不同前缀的编号（AK-47 这类）不该被当成账目。
        let noise = continuity_issues(&root, 3, "老周把 AK-47 靠在门边，复印件在桌上。");
        assert!(noise.is_empty(), "{noise:?}");
    }

    // ------------------------------------------------------------ 事件层：正文体检合并

    #[test]
    fn postcheck_reports_continuity_alongside_wordcount() {
        let root = event_book("ev-post");
        fs::write(root.join("大纲/事件/事件_002_码头交接.md"), EVENT_CARD).unwrap();
        write_outline(&root, 3, "事件 002 · 码头交接");
        fs::write(root.join("正文/第003章_对账.md"), "老周在码头等着。").unwrap();

        let check = postcheck_prose(&root, &root, 3, "run-1");
        let codes: Vec<&str> = check.issues.iter().map(|i| i.code.as_str()).collect();
        assert!(codes.contains(&"wordcount"), "{codes:?}");
        assert!(codes.contains(&"continuity"), "{codes:?}");
    }

    // ------------------------------------------------------------ 任务包缓存

    fn cache_book(name: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!("storydesk-pkt-{name}-{nanos}"));
        std::fs::create_dir_all(root.join("设定")).unwrap();
        std::fs::create_dir_all(root.join("大纲")).unwrap();
        std::fs::write(root.join("设定/角色状态.md"), "- 陈砚：项目经理\n").unwrap();
        root
    }

    /// 造一份任务包 + 缓存条目，模拟「上一次已经跑过 Python」。
    fn seed_packet(root: &Path, task: &str, chapters: Option<(u32, u32)>) -> PathBuf {
        let dir = root.join(".storydesk/prompts");
        std::fs::create_dir_all(&dir).unwrap();
        let packet = dir.join(format!("{task}-001.md"));
        std::fs::write(&packet, "# 任务包：生成第 1 章细纲\n").unwrap();
        let key = packet_cache_key(task, None, chapters);
        let digest = packet_sources_digest(root);
        packet_cache_store(root, key, digest, &packet);
        packet
    }

    #[test]
    fn packet_cache_key_distinguishes_task_volume_and_range() {
        let base = packet_cache_key("outline", None, Some((1, 1)));
        assert_ne!(base, packet_cache_key("outline", Some(2), Some((1, 1))));
        assert_ne!(base, packet_cache_key("outline", None, Some((1, 2))));
        assert_ne!(base, packet_cache_key("volume", None, Some((1, 1))));
        assert_eq!(base, packet_cache_key("outline", None, Some((1, 1))));
    }

    /// 命中的判定就是「不启 Python」：这里传的工作区压根没有 brief 脚本，
    /// 只要还去调脚本就一定报 env_missing；返回 Ok 即证明缓存直接短路了整段调用。
    #[test]
    fn packet_cache_hit_skips_python_entirely() {
        let root = cache_book("hit");
        let packet = seed_packet(&root, "outline", Some((1, 1)));

        let got = build_task_packet(&root, &root, "outline", None, Some((1, 1)))
            .expect("缓存命中不该再去找脚本");
        assert_eq!(PathBuf::from(&got.path), packet);
        assert!(got.raw.contains("缓存命中"), "{}", got.raw);

        // 第二次、第三次都命中（同章重跑 → Python 调用次数 0）。
        for _ in 0..2 {
            assert!(build_task_packet(&root, &root, "outline", None, Some((1, 1))).is_ok());
        }
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packet_cache_misses_after_a_source_file_changes() {
        let root = cache_book("source");
        seed_packet(&root, "outline", Some((1, 1)));
        let key = packet_cache_key("outline", None, Some((1, 1)));
        assert!(packet_cache_lookup(&root, &key, &packet_sources_digest(&root)).is_some());

        std::fs::write(
            root.join("设定/角色状态.md"),
            "- 陈砚：项目经理\n- 苏晚：法务\n",
        )
        .unwrap();

        assert!(
            packet_cache_lookup(&root, &key, &packet_sources_digest(&root)).is_none(),
            "设定变了必须重新生成任务包"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packet_cache_misses_for_another_chapter_range() {
        let root = cache_book("range");
        seed_packet(&root, "outline", Some((1, 1)));
        let digest = packet_sources_digest(&root);
        let hit = packet_cache_key("outline", None, Some((1, 1)));
        let miss = packet_cache_key("outline", None, Some((1, 2)));
        assert!(packet_cache_lookup(&root, &hit, &digest).is_some());
        assert!(
            packet_cache_lookup(&root, &miss, &digest).is_none(),
            "章区间不同就是另一份任务包"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packet_cache_misses_when_the_packet_file_is_gone() {
        let root = cache_book("gone");
        let packet = seed_packet(&root, "outline", Some((1, 1)));
        let key = packet_cache_key("outline", None, Some((1, 1)));
        let digest = packet_sources_digest(&root);
        assert!(packet_cache_lookup(&root, &key, &digest).is_some());
        std::fs::remove_file(&packet).unwrap();
        assert!(packet_cache_lookup(&root, &key, &digest).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packet_cache_misses_after_a_hand_rewrite() {
        let root = cache_book("rewrite");
        let packet = seed_packet(&root, "outline", Some((1, 1)));
        let key = packet_cache_key("outline", None, Some((1, 1)));
        let digest = packet_sources_digest(&root);
        // 模拟作者/上一版应用手改了任务包：mtime 变了就不能再当同一份用。
        std::thread::sleep(std::time::Duration::from_millis(12));
        std::fs::write(&packet, "# 任务包：被改过\n").unwrap();
        assert!(packet_cache_lookup(&root, &key, &digest).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn packet_cache_index_is_capped() {
        let root = cache_book("cap");
        let dir = root.join(".storydesk/prompts");
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..(PACKET_CACHE_MAX + 5) {
            let packet = dir.join(format!("outline-{i:03}.md"));
            std::fs::write(&packet, "x\n").unwrap();
            let key = packet_cache_key("outline", None, Some((i as u32 + 1, i as u32 + 1)));
            let digest = packet_sources_digest(&root);
            packet_cache_store(&root, key, digest, &packet);
        }
        let cache = read_packet_cache(&root);
        assert_eq!(cache.entries.len(), PACKET_CACHE_MAX);
        std::fs::remove_dir_all(&root).ok();
    }

    /// 把文件 mtime 往前推，模拟「很久以前生成的旧任务包」。
    fn age_file(path: &Path, secs_ago: u64) {
        let when = std::time::SystemTime::now() - std::time::Duration::from_secs(secs_ago);
        let file = std::fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(
            std::fs::FileTimes::new()
                .set_modified(when)
                .set_accessed(when),
        )
        .unwrap();
    }

    /// 细纲写了「正文目标字数：6200 字（区间 6000—6500）」，体检就得按 6000 当底线：
    /// 事故是细纲要 6200、写手交 5389，旧实现只比 800，于是报「通过」。
    #[test]
    fn postcheck_uses_the_outline_target_as_the_word_floor() {
        let root = cache_book("floor");
        fs::create_dir_all(root.join("大纲")).unwrap();
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("大纲/细纲_第003章.md"),
            "# 第003章 细纲\n\n正文目标字数：6200 字（全章总字数；区间 6000—6500）\n",
        )
        .unwrap();
        let body = "他走进库房，把饭票翻过来。".repeat(20);
        fs::write(
            root.join("正文/第003章_库房.md"),
            format!("# 第003章 库房\n\n{body}\n\n（本章完）\n"),
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let post = postcheck_prose(&workspace, &root, 3, "t-floor");
        let issue = post
            .issues
            .iter()
            .find(|i| i.code == "wordcount")
            .expect("细纲要 6200 字，只有三百多字必须报不足");
        assert!(issue.text.contains("6000-6500"), "{}", issue.text);
    }

    /// 没有细纲（新书第一章）才退回 800 的兜底口径。
    #[test]
    fn postcheck_falls_back_to_800_without_an_outline() {
        let root = cache_book("no-floor");
        fs::create_dir_all(root.join("正文")).unwrap();
        fs::write(
            root.join("正文/第001章_夜雨来客.md"),
            "# 第001章 夜雨来客\n\n很短的一章。\n",
        )
        .unwrap();
        let workspace = root.parent().unwrap().to_path_buf();

        let post = postcheck_prose(&workspace, &root, 1, "t-800");
        let issue = post
            .issues
            .iter()
            .find(|i| i.code == "wordcount")
            .expect("不足 800 字要报");
        assert!(issue.text.contains("800"), "{}", issue.text);
    }

    #[test]
    fn generated_packet_names_are_recognised_and_user_files_are_not() {
        assert!(is_generated_packet_name("outline-001.md"));
        assert!(is_generated_packet_name("world-000.md"));
        assert!(is_generated_packet_name("volume-012.md"));
        assert!(!is_generated_packet_name("我的细纲-最终版.md"));
        assert!(!is_generated_packet_name("outline-新想法.md"));
        assert!(!is_generated_packet_name("outline-1.md"));
        assert!(!is_generated_packet_name("outline-0001.md"));
        assert!(!is_generated_packet_name("outline-001.txt"));
        assert!(!is_generated_packet_name("大纲.md"));
    }

    /// 缓存索引里被塞进路径穿越时，清理逻辑不能顺着它删到别的目录。
    #[test]
    fn cleanup_refuses_paths_outside_the_cache_dir() {
        let root = cache_book("traversal");
        std::fs::create_dir_all(root.join("正文")).unwrap();
        let victim = root.join("正文/第001章_雨夜来客.md");
        std::fs::write(&victim, "正文\n").unwrap();
        let sneaky = root.join(".storydesk/prompts/../../../正文/第001章_雨夜来客.md");
        assert!(!is_packet_in_cache_dir(&root, &sneaky));
        remove_packet_file(&root, &sneaky);
        assert!(victim.is_file(), "路径穿越不能借清理删掉正文");
        // 正常任务包则认得出来。
        let packet = root.join(".storydesk/prompts/outline-001.md");
        std::fs::create_dir_all(packet.parent().unwrap()).unwrap();
        std::fs::write(&packet, "x\n").unwrap();
        assert!(is_packet_in_cache_dir(&root, &packet));
        std::fs::remove_dir_all(&root).ok();
    }

    /// 索引被淘汰时，对应的任务包文件要跟着删掉（不只是删索引条目）。
    #[test]
    fn evicting_an_entry_deletes_its_packet_file() {
        let root = cache_book("evict");
        let dir = root.join(".storydesk/prompts");
        std::fs::create_dir_all(&dir).unwrap();
        let mut packets = Vec::new();
        for i in 0..(PACKET_CACHE_MAX + 3) {
            let packet = dir.join(format!("outline-{i:03}.md"));
            std::fs::write(&packet, "x\n").unwrap();
            let key = packet_cache_key("outline", None, Some((i as u32 + 1, i as u32 + 1)));
            let digest = packet_sources_digest(&root);
            packet_cache_store(&root, key, digest, &packet);
            packets.push(packet);
        }
        let cache = read_packet_cache(&root);
        assert_eq!(cache.entries.len(), PACKET_CACHE_MAX);
        // 最老的 3 份：索引里没有、文件也不在。
        for packet in packets.iter().take(3) {
            assert!(
                !packet.is_file(),
                "被淘汰的任务包必须删掉：{}",
                packet.display()
            );
        }
        // 最新的一份必须还在，而且仍能被命中。
        let last = packets.last().unwrap();
        assert!(last.is_file(), "当前有效任务包不能被删");
        let live: Vec<String> = cache.entries.values().map(|e| e.path.clone()).collect();
        assert!(live.iter().any(|p| PathBuf::from(p) == *last));
        std::fs::remove_dir_all(&root).ok();
    }

    /// 清理必须幂等，且绝不碰用户自己的文件和仍在索引里的任务包。
    #[test]
    fn orphan_sweep_keeps_user_files_and_live_packets() {
        let root = cache_book("sweep");
        let dir = root.join(".storydesk/prompts");
        std::fs::create_dir_all(&dir).unwrap();

        // 1) 活着的任务包（索引里有）
        let live = dir.join("outline-001.md");
        std::fs::write(&live, "# 任务包\n").unwrap();
        let digest = packet_sources_digest(&root);
        packet_cache_store(
            &root,
            packet_cache_key("outline", None, Some((1, 1))),
            digest,
            &live,
        );

        // 2) 孤儿任务包（索引里没有，30 分钟前生成的）
        let orphan = dir.join("outline-006.md");
        std::fs::write(&orphan, "# 旧任务包\n").unwrap();
        age_file(&orphan, 3600);

        // 3) 用户自己放的文件：名字不符合生成格式，多旧都不能删
        let mine = dir.join("我的细纲-最终版.md");
        std::fs::write(&mine, "# 我写的\n").unwrap();
        age_file(&mine, 3600);
        // 4) 刚生成的孤儿：在宽限期内，这一轮不动它
        let fresh = dir.join("outline-011.md");
        std::fs::write(&fresh, "# 刚生成\n").unwrap();

        // 再存一次（触发 sweep），并且连跑两轮验证幂等。
        for _ in 0..2 {
            let digest = packet_sources_digest(&root);
            let next = dir.join("outline-016.md");
            std::fs::write(&next, "# 新任务包\n").unwrap();
            packet_cache_store(
                &root,
                packet_cache_key("outline", None, Some((16, 16))),
                digest,
                &next,
            );
        }

        assert!(live.is_file(), "索引里还有的任务包不能被删");
        assert!(!orphan.is_file(), "过期孤儿任务包应被清理");
        assert!(mine.is_file(), "用户手写文件绝不能被清理");
        assert!(fresh.is_file(), "宽限期内的任务包不动");
        std::fs::remove_dir_all(&root).ok();
    }

    /// 真实书验证（会起 Python，默认不跑）：
    ///
    /// ```powershell
    /// $env:STORYDESK_WS="D:\桌面\大制作\xs\我的作品集\长篇\《隐藏条款》"
    /// $env:STORYDESK_BOOK="D:\桌面\大制作\xs\我的作品集\长篇\《隐藏条款》"
    /// cargo test --lib packet_cache_real_book -- --ignored --nocapture
    /// ```
    #[test]
    #[ignore]
    fn packet_cache_real_book_from_env() {
        let (Ok(ws), Ok(book)) = (
            std::env::var("STORYDESK_WS"),
            std::env::var("STORYDESK_BOOK"),
        ) else {
            println!("跳过：需要 STORYDESK_WS 与 STORYDESK_BOOK");
            return;
        };
        let ws = PathBuf::from(ws);
        let book = PathBuf::from(book);

        let t0 = std::time::Instant::now();
        let first = build_task_packet(&ws, &book, "outline", None, Some((1, 1))).expect("第一次");
        println!(
            "第一次：{} ms → {}（{}）",
            t0.elapsed().as_millis(),
            first.path,
            first.raw.trim()
        );

        let t1 = std::time::Instant::now();
        let second = build_task_packet(&ws, &book, "outline", None, Some((1, 1))).expect("第二次");
        println!(
            "第二次：{} ms → {}（{}）",
            t1.elapsed().as_millis(),
            second.path,
            second.raw.trim()
        );
        assert!(
            second.raw.contains("缓存命中"),
            "同一章重跑必须命中缓存，Python 调用次数应为 0"
        );
    }
}
