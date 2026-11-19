//! AI 引擎相关命令：启动/取消运行、门禁探测与调用、API Key 管理。
//!
//! 一次 `ai_start` 的完整闭环：
//! 1. 路径校验（`path_guard`）；
//! 2. 前置门禁：写正文/去味时先跑 `storyctl.py chapter check`，不过就直接发 `hook://blocked`，
//!    根本不启动引擎（省时间，也让用户看到真实原因）；
//! 3. 生成任务包（`build_architect_brief.py`），拿它的路径去填 prompt 槽位；
//! 4. 按 `agent-calls.md` 的模板拼 prompt（字词不改），交给引擎适配器；
//! 5. 运行期间：chunk 走事件；结束时统一由这里发 `ai://done`，若 exit 2 再发 `hook://blocked`。

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::models::ai::{self, AgentConfig, AgentInfo, AgentRole};
use crate::models::dto::{
    EngineInfo, EngineKind, GateInfo, GateOutput, HookBlock, PostCheck, RunDone, RunKind,
    RunRequest, TaskPacket,
};
use crate::models::StoryError;
use crate::services::engine::{self, EngineState, RunCtx, RunOutcome};
use crate::services::{
    context, env_check, gate, hook_parse, intent, memory, path_guard, settings, workspace,
};

/// 一次 `ai_start` 的返回。
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StartResult {
    pub run_id: String,
    pub engine: String,
    pub chapter: Option<u32>,
    /// 实际送进 CLI 的 prompt（回传给 UI 供核对；不含任何密钥）
    pub prompt: String,
    pub task_packet: Option<String>,
    pub gate: Option<GateOutput>,
    /// 前置门禁未通过时带回来，前端直接渲染卡片（此时没有启动引擎）
    pub blocked: Option<HookBlock>,
    /// 非致命提示：任务包没生成、门禁脚本缺失等
    pub note: Option<String>,
    /// 本条由哪个智能体执行（没有智能体时为 null）
    pub agent: Option<String>,
}

/// 严格按 `skills/story-long-write/references/agent-calls.md` 的模板拼 prompt。
///
/// 前三行 + 最后一行是契约原文，字词不可改；`{book_root}` / `{task_packet_path}` 用真实值填充。
pub fn build_prompt(book_root: &Path, task_packet: Option<&Path>, note: Option<&str>) -> String {
    let packet = task_packet
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "（未生成任务包：直接按 设定/ 与 大纲/ 的既有内容执行）".to_string());

    let mut prompt = format!(
        "项目目录：{}\n任务包：{}\n先完整读取任务包，按包里的流程与模板完成；作者已定的方向、设定和要求都在 设定/，对话内容不会传给你\n交付后只回任务包开头要求的几项",
        book_root.display(),
        packet
    );

    if let Some(extra) = note.map(str::trim).filter(|s| !s.is_empty()) {
        prompt.push_str("\n补充要求：");
        prompt.push_str(extra);
    }
    prompt
}

/// 智能体聚焦目录：前端传的值必须在白名单里，否则用该智能体配置里的目录。
///
/// 不接受绝对路径、不接受 `..`：target_dir 只当「书根下的一个目录名」用。
pub(crate) fn pick_dir(agent: &AgentConfig, raw: Option<&str>) -> String {
    let cleaned = raw
        .map(str::trim)
        .map(|s| s.trim_end_matches(['/', '\\']))
        // 口径统一到 path_guard::rel_is_safe：以前这里挡 .. 和冒号，
        // C:foo（盘符相对）与 C:/x 这种照样能过，随后 book_root.join() 会把
        // 基准整个换掉。白名单（is_known_dir）是第二道闸，两套规则不该并存。
        .filter(|s| crate::services::path_guard::rel_is_safe(s))
        .unwrap_or("");
    if ai::is_known_dir(cleaned) {
        cleaned.to_string()
    } else {
        agent.target_dir.clone()
    }
}

/// 文件名里的章号（`细纲_第012章.md` → 12；`大纲.md` → None）。
///
/// 只认「第 N 章」这种写法：正文 / 大纲 / 细纲三个目录的文件名都由项目自己生成，
/// 其余格式（`012.md`）分不清是章号还是序号，宁可当「无章号」走兜底，也不猜错章。
pub(crate) fn chapter_of_name(name: &str) -> Option<u32> {
    let chars: Vec<char> = name.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if *c != '第' {
            continue;
        }
        let mut j = i + 1;
        let mut digits = String::new();
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        while j < chars.len() && chars[j].is_ascii_digit() {
            digits.push(chars[j]);
            j += 1;
        }
        while j < chars.len() && chars[j].is_whitespace() {
            j += 1;
        }
        if digits.is_empty() || j >= chars.len() || chars[j] != '章' {
            continue;
        }
        if let Ok(no) = digits.parse::<u32>() {
            if no > 0 {
                return Some(no);
            }
        }
    }
    None
}

/// 排序键：当前章(0) → 相邻章(1) → 无章号的全局大纲(2) → 更远的章(3, 章号差)。
///
/// 无章号文件（`大纲.md`）排在「相邻章」之后、「较远章」之前：全书大纲只有一份，
/// 掉了模型就丢了整本书的结构；但紧邻的前后章细纲对本章连贯性的影响更大。
fn proximity(rel: &str, chapter: u32) -> (u8, u32) {
    let name = rel.rsplit('/').next().unwrap_or(rel);
    match chapter_of_name(name) {
        Some(no) if no == chapter => (0, 0),
        Some(no) if no.abs_diff(chapter) == 1 => (1, 0),
        Some(no) => (3, no.abs_diff(chapter)),
        None => (2, 0),
    }
}

/// 上下文文件清单：前端抓到的优先，抓不到就自己扫一遍目录（最多 [`engine::CONTEXT_LIMIT`] 个 `.md`）。
///
/// 只回相对书根的路径（`设定/角色/林昭.md`），prompt 里可读、也不泄露机器上的绝对路径。
///
/// 知道「当前在处理第几章」时按章节距离挑（当前 → 相邻 → 无章号的全局大纲 → 更远）；
/// 不知道（`None`）时无章号的全局大纲排在最前，避免被几百个按章编号的细纲挤掉。
///
/// 实测事故：`大纲/` 有 301 个细纲，旧代码按文件名取前 40 个，写到第 200 章时
/// 模型手里仍然是第 1–39 章的细纲，于是它照着开头的设定往下编。
pub(crate) fn collect_context_files_near(
    book_root: &Path,
    dir: &str,
    given: &[String],
    chapter: Option<u32>,
) -> Vec<String> {
    // 前端清单只当「候选」：真的存在、是 .md、不带 .. 的才留下。
    // 旧实现原样返回，过期/改名/手写的路径会直接进 prompt——模型拿到一条读不到的文件，
    // 只能凭开头几章编，和「按字典序取前 40 个」是同一个病。
    let mut files: Vec<String> = Vec::new();
    let mut stale = 0usize;
    for raw in given {
        let rel = raw.trim().replace('\\', "/");
        let usable = !rel.is_empty()
            && !rel.contains("..")
            && rel.to_ascii_lowercase().ends_with(".md")
            && book_root.join(&rel).is_file();
        if usable {
            files.push(rel);
        } else {
            stale += 1;
        }
    }
    if stale > 0 {
        eprintln!("[storydesk] 前端上下文清单里 {stale} 条用不上（不存在或不是 .md），已忽略");
    }
    // 后端兜底：递归扫 `<book>/<dir>`，深度最多 3 层。
    // `SCAN_GUARD` 只防病态目录（几万个文件）扫盘，不是取用上限：
    // 取用上限在下面按章节距离挑完再截断，先截断就永远只看得到字典序开头。
    const SCAN_GUARD: usize = 4000;
    fn walk(base: &Path, rel: &str, depth: usize, out: &mut Vec<String>) {
        if depth > 3 || out.len() >= SCAN_GUARD {
            return;
        }
        let Ok(entries) = std::fs::read_dir(base.join(rel)) else {
            return;
        };
        let mut names: Vec<_> = entries.flatten().collect();
        names.sort_by_key(|e| e.file_name());
        for entry in names {
            if out.len() >= SCAN_GUARD {
                return;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with('.') {
                continue;
            }
            let child = format!("{rel}/{name}");
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                walk(base, &child, depth + 1, out);
            } else if name.to_ascii_lowercase().ends_with(".md") {
                out.push(child);
            }
        }
    }

    // 前端清单为空（或整份都用不上）才扫目录：两路进来的清单必须走同一套排序与截断，
    // 否则「前端给的顺序」会绕过按章节距离挑选，又变回字典序前 40 个。
    if files.is_empty() {
        walk(book_root, dir, 1, &mut files);
    }
    files.sort();
    // 去重按小写比较：Windows 上 大纲/A.md 与 大纲/a.md 是同一个文件。
    let mut seen = std::collections::HashSet::new();
    files.retain(|p| seen.insert(p.to_ascii_lowercase()));
    match chapter {
        Some(no) => files.sort_by(|a, b| {
            proximity(a, no)
                .cmp(&proximity(b, no))
                .then_with(|| a.cmp(b))
        }),
        // 不知道当前章时，「无章号」的全书级文件（`大纲.md`）排前面：
        // 它只有一份，被 300 个编号细纲按字典序挤掉就等于丢了整本书的结构。
        None => files.sort_by(|a, b| {
            let numbered = |p: &str| chapter_of_name(p.rsplit('/').next().unwrap_or(p)).is_some();
            numbered(a).cmp(&numbered(b)).then_with(|| a.cmp(b))
        }),
    }
    if files.len() > engine::CONTEXT_LIMIT {
        files.truncate(engine::CONTEXT_LIMIT);
    }
    files
}

/// 有没有角色就拼智能体段落；返回拼好的 prompt 与实际生效的智能体。
///
/// 配置来自 `settings.agents`（用户可在设置里改名称/头像/目录/提示词），
/// 所以「改了 Prompt 下一次运行就生效」不需要用户重启应用。
///
/// 阶段 10 起多两件事：把 `设定/追踪/参考资料/本章细纲` 这些**全局必读**文件挑出来塞进
/// prompt（[`context::global_context`]），以及按任务文本判定是否只读
/// （[`ai::is_read_only_task`] → 摘掉 Write/Edit/MultiEdit）。
pub(crate) fn apply_agent_prompt(
    base: String,
    role: Option<AgentRole>,
    target_dir: Option<&str>,
    context_files: &[String],
    book_root: &Path,
    agents: &[AgentConfig],
    allowed_tools: Option<&[String]>,
    chapter: u32,
    task: Option<&str>,
    topic: Option<&str>,
) -> (String, Option<AgentConfig>) {
    let Some(role) = role else {
        return (base, None);
    };
    let mut agent = ai::resolve_agent(agents, role.as_str());
    agent.target_dir = pick_dir(&agent, target_dir);
    let files =
        collect_context_files_near(book_root, &agent.target_dir, context_files, Some(chapter));
    let global = context::global_context(book_root, chapter, &files);
    let read_only = task.map(ai::is_read_only_task).unwrap_or(false);
    let mut tools = ai::effective_tools(&agent, allowed_tools);
    if read_only {
        tools = ai::read_only_tools(&tools);
    }
    // 阶段 13（记忆功能）：上一章的前情提要（细纲要点 + 开头/结尾），写正文/细纲时注入。
    let prev_digest = context::prev_chapter_digest(book_root, chapter);
    // R3 项 6：单次运行也走设定快照（源文件没变只是一次字符串比较，不重复写盘）。
    let snapshot_abs =
        crate::services::setting_snapshot::ensure(book_root).map(|report| report.abs);
    let mut prompt = engine::compose_agent_prompt(
        &agent,
        &tools,
        &files,
        &global,
        read_only,
        topic,
        &base,
        prev_digest.as_deref(),
        snapshot_abs.as_deref(),
    );
    // 全书级文风体检：单章评审看不见「同一句式全书出现几百次」这件事，只有摊开成数字
    // 才看得见。这里只把**计数事实**接在写手的 prompt 末尾，判断仍由模型做（见
    // services/stylestat.rs 的模块注释与 docs/纪律清单.md 第 9 条）。
    // 只在写正文时注入：大纲、设定、审阅不需要这份数字，白占上下文。
    if agent.target_dir.trim_end_matches(['/', '\\']) == "正文" {
        if let Some(stats) = crate::services::stylestat::stats_of(book_root) {
            prompt.push_str("\n\n");
            prompt.push_str(&crate::services::stylestat::brief(&stats, 5));
        }
    }
    (prompt, Some(agent))
}

/// 下一章章号：扫 `正文/第NNN章_*.md` 取 max + 1。
pub(crate) fn next_chapter_no(book_root: &Path) -> u32 {
    let dir = book_root.join("正文");
    let mut max = 0u32;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(rest) = name.strip_prefix('第') {
                let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
                if let Ok(n) = digits.parse::<u32>() {
                    max = max.max(n);
                }
            }
        }
    }
    max + 1
}

pub(crate) fn run_id_now() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("run-{nanos}")
}

pub(crate) fn resolve_pair(
    workspace_root: &str,
    book_root: &str,
) -> Result<(PathBuf, PathBuf), StoryError> {
    // path_guard 返回的是 canonicalize 后的路径（Windows 上带 `\\?\` 前缀）。
    // 送进 prompt 和送给用户看的必须是干净路径，所以这里统一剥掉前缀。
    let ws = workspace::strip_verbatim(path_guard::validate_path(
        Path::new(workspace_root),
        Path::new(workspace_root),
    )?);
    let book = workspace::strip_verbatim(path_guard::validate_path(
        Path::new(workspace_root),
        Path::new(book_root),
    )?);
    Ok((ws, book))
}

// ---------------------------------------------------------------- 运行

/// 把一行补充说明拼到已有文本后面（空则直接当正文）。
fn join_line(base: &str, line: &str) -> String {
    if base.trim().is_empty() {
        line.to_string()
    } else {
        format!("{}\n{line}", base.trim_end())
    }
}

#[tauri::command]
pub async fn ai_start(app: AppHandle, req: RunRequest) -> Result<StartResult, StoryError> {
    let (ws, book) = resolve_pair(&req.workspace_root, &req.book_root)?;
    // 单智能体运行也要装/升级 hook：第 037 章就是写手子智能体绕开 Write、用 PowerShell 改正文写坏的，
    // 旧 hook 的 matcher 只有 Write|Edit|MultiEdit。编排流程里已经有这一步，这里补上，
    // 免得不走编排直接跑写手时没有防护（ensure 是幂等的，会把旧 matcher 升级成含 Bash 的版本）。
    match crate::services::dir_guard::ensure(&book) {
        Err(e) => engine::log_failure(&app, "hook 没装上（不挡执行）", &e),
        Ok(_) => {
            if let Some(note) = crate::services::dir_guard::isolation_degraded(&book) {
                engine::log_failure(&app, "目录隔离降级", note);
            }
        }
    }
    let engine_kind = req.engine.unwrap_or(EngineKind::Claude);
    let chapter = req
        .chapter
        .or_else(|| req.note.as_deref().and_then(intent::chapter_of))
        .or_else(|| Some(next_chapter_no(&book)));
    let run_id = run_id_now();

    let scripts = gate::resolve(&ws);
    let mut notes: Vec<String> = Vec::new();
    let mut gate_output: Option<GateOutput> = None;

    // ---- 1) 前置门禁
    // 写正文的前置只该管「细纲在不在」：装了 hook 就跑 hook，没装 hook 就本地同口径检查。
    // 去 AI 味处理的是已存在的正文，才用 storyctl.py chapter check（那是写完之后的体检）。
    if req.kind.needs_gate() {
        if let Some(ch) = chapter {
            let checked = match req.kind {
                RunKind::Prose => Some(gate::precheck_prose(&ws, &book, ch)),
                _ if scripts.gate_ready() => Some(gate::chapter_check(&ws, &book, ch)),
                _ => {
                    notes.push("未检测到 storyctl.py，跳过前置门禁".to_string());
                    None
                }
            };

            if let Some(checked) = checked {
                match checked {
                    Ok(out) => {
                        if !out.ok {
                            let block = hook_parse::gate_block(&run_id, &out, ch);
                            let raw = block.stderr.clone();
                            let _ = app.emit(engine::BLOCKED_EVENT, block.clone());
                            let _ = app.emit(
                                engine::DONE_EVENT,
                                RunDone {
                                    run_id: run_id.clone(),
                                    exit: 2,
                                    status: "blocked".to_string(),
                                    duration_ms: 0,
                                    stderr: Some(raw),
                                    detail: Some("前置门禁未通过，未启动引擎".to_string()),
                                },
                            );
                            return Ok(StartResult {
                                run_id,
                                engine: engine_kind.as_str().to_string(),
                                chapter,
                                prompt: String::new(),
                                task_packet: None,
                                gate: Some(out),
                                blocked: Some(block),
                                note: None,
                                agent: req.agent_role.map(|r| ai::spec(r).name.to_string()),
                            });
                        }
                        // 提示性放行（事件层缺卡）：不拦，但要让单章运行也看见这一章按什么规矩写。
                        if out.stdout.trim().starts_with("事件层提示") {
                            notes.push(out.stdout.trim().to_string());
                        }
                        gate_output = Some(out);
                    }
                    Err(e) => notes.push(format!("章节体检没跑成：{e}")),
                }
            }
        }
    }

    // ---- 2) 任务包
    let mut packet_path: Option<PathBuf> = None;
    if req.kind == RunKind::Outline {
        if scripts.brief_ready() {
            match gate::build_task_packet(&ws, &book, "outline", None, chapter.map(|c| (c, c))) {
                Ok(packet) => packet_path = Some(PathBuf::from(packet.path)),
                Err(e) => notes.push(format!("任务包未生成：{e}")),
            }
        } else {
            notes.push("未检测到 build_architect_brief.py，任务包留空".to_string());
        }
    }

    // ---- 3) prompt + 引擎
    // 四行契约先按原样拼好，再由智能体段落前置包一层（契约字词不动）。
    // 阶段 10：写正文时把细纲里的字数区间/比例要求抽出来，接在「补充要求」末尾
    // ——这是 prompt 的最后一行，模型最不容易忽略的位置。
    // 用户点名了字数时，细纲里的字数必须让位，用户字数最后再钉一次。
    let mut note = req.note.clone();
    if req.kind == RunKind::Prose {
        let chapter_no = chapter.unwrap_or(1);
        let note_text = note.as_deref().unwrap_or("");
        let word_count =
            intent::clause_word_count_of(note_text).or_else(|| intent::word_count_of(note_text));
        let extra = if word_count.is_some() {
            context::hard_constraints_without_word_count(&book, chapter_no)
        } else {
            context::hard_constraints(&book, chapter_no)
        };
        if let Some(extra) = extra {
            note = Some(
                match note.map(|n| n.trim().to_string()).filter(|n| !n.is_empty()) {
                    Some(task) => format!("{task}；{extra}"),
                    None => extra,
                },
            );
        }
        if let Some(wc) = word_count {
            let clause = intent::word_count_clause(Some(wc), false);
            note = Some(match note {
                Some(text) if text.contains("字数硬要求（最高优先级）") => text,
                Some(text) => format!("{text}；{clause}"),
                None => clause,
            });
        }
        let lock = format!(
            "【章号锁定】本步只允许产出或覆盖「第{chapter_no:03}章_」开头的这一个文件；\
             正文/ 下的其它章节文件一律不要写入、修改、续写或重写。"
        );
        note = Some(match note {
            Some(text) if text.contains("章号锁定") => text,
            Some(text) => format!("{text}；{lock}"),
            None => lock,
        });
    }
    let contract = build_prompt(&book, packet_path.as_deref(), note.as_deref());
    let cfg = settings::load(&app);
    let agents = cfg.agents.clone();
    let (prompt, agent) = apply_agent_prompt(
        contract,
        req.agent_role,
        req.target_dir.as_deref(),
        &req.context_files,
        &book,
        &agents,
        cfg.allowed_tools.as_deref(),
        chapter.unwrap_or(1),
        note.as_deref(),
        // 阶段 11.5：单发模式下题材从任务文本里抽（拿不到就不写这一段）。
        intent::classify(note.as_deref().unwrap_or_default())
            .genre
            .as_deref(),
    );
    let agent_name = agent.as_ref().map(|a| a.name.clone());
    if let Some(agent) = &agent {
        // 验收要看得到：终端里能看到「哪个智能体 + 它这次用的提示词 + 注入后的 prompt 头部」。
        let head: Vec<&str> = prompt.lines().take(10).collect();
        let tools = ai::effective_tools(agent, cfg.allowed_tools.as_deref());
        eprintln!("[storydesk] 智能体：{}（{}）", agent.name, agent.id);
        eprintln!("[storydesk] 角色提示词：{}", agent.system_prompt);
        eprintln!(
            "[storydesk] 允许的工具：{}",
            if tools.is_empty() {
                "（一个都没勾）".to_string()
            } else {
                tools.join("、")
            }
        );
        eprintln!("[storydesk] prompt 前 10 行：\n{}", head.join("\n"));
    }

    // 动态环境注入：按「当前激活的第三方提供商」决定变量名与 base URL。
    // Key 只经 EnvPlan → Command::env 进子进程，绝不出现在参数、日志与返回值里。
    // provider 本体也要带走：opencode 引擎要按它生成 opencode.json（自定义 API 映射）。
    let (env_plan, provider) = match settings::active_provider_with_key(&app) {
        Some((provider, key)) => {
            let plan = engine::EnvPlan::for_provider(&provider, key);
            if let Some(note) = plan.note.clone() {
                notes.push(note);
            }
            (plan, Some(provider))
        }
        None => {
            notes.push("还没选择模型提供商，本次不注入任何环境变量".to_string());
            (engine::EnvPlan::none(), None)
        }
    };

    let adapter = engine::build(
        engine_kind,
        env_plan,
        provider.as_ref(),
        settings::load(&app).opencode_path.as_deref(),
    )?;
    let state = app.state::<EngineState>();
    let handle = state.insert(&run_id);

    let ctx = RunCtx {
        workspace_root: ws.clone(),
        book_root: book.clone(),
        task_packet: packet_path.clone(),
        kind: req.kind,
        chapter,
        cancel: handle.cancel.clone(),
        pid: handle.pid.clone(),
        capture: None,
    };

    let kickoff = app.clone();
    let kickoff_run = run_id.clone();
    let prompt_for_run = prompt.clone();
    let engine_name = adapter.name().to_string();
    let kind_for_post = req.kind;
    let ws_for_post = ws.clone();
    let book_for_post = book.clone();
    let chapter_for_post = chapter;

    tauri::async_runtime::spawn(async move {
        // RAII 收尾：这个任务无论怎么结束（正常返回 / 提前 return / panic），
        // 都会把这次 run 从运行表里摘掉，不会留下永久「执行中」。
        let task_state = kickoff.state::<EngineState>();
        let _slot = engine::RunSlot::adopt(&task_state, &kickoff_run);
        let started = Instant::now();
        let result = adapter
            .run(prompt_for_run, ctx, kickoff.clone(), kickoff_run.clone())
            .await;
        let duration = started.elapsed().as_millis() as u64;

        match result {
            Ok(outcome) => {
                // 先发阻断卡片，再发终局，UI 上卡片不会闪在「已完成」之后
                if let RunOutcome::Blocked { stderr, .. } = &outcome {
                    let block = hook_parse::to_block(&kickoff_run, stderr);
                    let _ = kickoff.emit(engine::BLOCKED_EVENT, block);
                }
                let cancelled = matches!(&outcome, RunOutcome::Failed { stderr, .. } if stderr.contains(engine::CANCEL_MARK));
                engine::emit_done(
                    &kickoff,
                    &kickoff_run,
                    &outcome,
                    duration,
                    if cancelled {
                        Some("用户取消".to_string())
                    } else {
                        None
                    },
                );

                // ---- 后置体检
                // 选在**命令层**做，不塞进适配器：适配器只管「怎么跑 CLI」，
                // 体检属于 oh-story 的门禁语义，跟用哪个引擎无关。
                let prose_done = matches!(&outcome, RunOutcome::Done { .. })
                    && kind_for_post == RunKind::Prose
                    && !cancelled;
                if prose_done {
                    let run_for_check = kickoff_run.clone();
                    let chapter =
                        chapter_for_post.unwrap_or_else(|| next_chapter_no(&book_for_post));

                    // 正文格式后处理：模型会把细纲的幕序照抄成「## 一」「## 场景一」。
                    // 先剥掉再体检——字数与粘连检查都该基于读者实际读到的文本。
                    let clean_book = book_for_post.clone();
                    let clean_report = crate::commands::blocking(move || {
                        crate::services::prose_clean::strip_chapter_headings(&clean_book, chapter)
                    })
                    .await
                    .ok()
                    .flatten();
                    let ws_check = ws_for_post.clone();
                    let book_check = book_for_post.clone();
                    let checked = crate::commands::blocking(move || {
                        Ok(gate::postcheck_prose(
                            &ws_check,
                            &book_check,
                            chapter,
                            &run_for_check,
                        ))
                    })
                    .await;
                    let mut post = match checked {
                        Ok(post) => post,
                        Err(e) => PostCheck::skipped(
                            &kickoff_run,
                            chapter,
                            &format!("后置体检没跑成：{e}"),
                        ),
                    };

                    if let Some(report) = clean_report {
                        let line = format!(
                            "[正文清理] 去掉 {} 处小标题标记（{}）。",
                            report.removed,
                            report.samples.join(" / ")
                        );
                        post.stdout = join_line(&post.stdout, &line);
                    }

                    // ---- 正文清理：把「（本章完）」之后的创作注释搬到 追踪/写作备注.md
                    //
                    // 放在体检之后、emit 之前：体检的字数口径保持原样（历史数字可比），
                    // 但卡片上会多一行「已搬到…」，用户知道正文被程序动过、动到哪去了。
                    match crate::commands::blocking(move || {
                        memory::detach_writer_notes(&ws_for_post, &book_for_post, chapter)
                    })
                    .await
                    {
                        Ok(Some(report)) => {
                            let line = format!(
                                "[正文清理] 第{}章的创作注释（{} 字）已搬到 {}，正文只留故事。",
                                report.chapter, report.moved_chars, report.notes_rel
                            );
                            post.stdout = join_line(&post.stdout, &line);
                        }
                        Ok(None) => {}
                        Err(e) => {
                            post.stdout =
                                join_line(&post.stdout, &format!("[正文清理] 没搬成：{e}"));
                        }
                    }

                    let _ = kickoff.emit(engine::POSTCHECK_EVENT, post);
                }
            }
            Err(e) => {
                let _ = kickoff.emit(
                    engine::DONE_EVENT,
                    RunDone {
                        run_id: kickoff_run.clone(),
                        exit: -1,
                        status: "error".to_string(),
                        duration_ms: duration,
                        stderr: Some(e.to_string()),
                        detail: Some(format!("{engine_name} 启动失败")),
                    },
                );
            }
        }

    });

    Ok(StartResult {
        run_id,
        engine: engine_kind.as_str().to_string(),
        chapter,
        prompt,
        task_packet: packet_path.map(|p| p.display().to_string()),
        gate: gate_output,
        blocked: None,
        note: if notes.is_empty() {
            None
        } else {
            Some(notes.join("；"))
        },
        agent: agent_name,
    })
}

/// 智能体团队：六张卡片的权威数据（前端的 emoji/名称/目录都来自这里）。
#[tauri::command]
pub fn agent_specs() -> Vec<AgentInfo> {
    ai::all_specs().iter().map(|s| s.info()).collect()
}

/// 按角色 id 取一份档案；未知 id 返回 null（前端可据此提示「没有这个智能体」）。
#[tauri::command]
pub fn agent_spec(role: String) -> Option<AgentInfo> {
    AgentRole::parse(&role).map(|r| ai::spec(r).info())
}

/// 取消一次运行：置位标志，并在 Windows 上用 taskkill 杀整棵进程树。
#[tauri::command]
pub async fn ai_cancel(app: AppHandle, run_id: String) -> Result<bool, StoryError> {
    let state = app.state::<EngineState>();
    let Some(handle) = state.cancel(&run_id) else {
        return Ok(false);
    };

    let pid = handle.pid.lock().ok().and_then(|slot| *slot);
    if let Some(pid) = pid {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let _ = std::process::Command::new("taskkill")
                .args(["/T", "/F", "/PID", &pid.to_string()])
                .creation_flags(0x0800_0000)
                .output();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("kill")
                .args(["-9", &pid.to_string()])
                .output();
        }
    }

    state.take(&run_id);
    Ok(true)
}

/// 当前在跑的 run（调试面板用）。
#[tauri::command]
pub async fn ai_running(app: AppHandle) -> Result<Vec<String>, StoryError> {
    Ok(app.state::<EngineState>().running())
}

/// 引擎可用性列表（设置页引擎区 + AI 面板顶部）。
#[tauri::command]
pub async fn ai_engines(app: AppHandle) -> Result<Vec<EngineInfo>, StoryError> {
    let claude = env_check::which_exe("claude");
    let opencode = crate::services::opencode_setup::locate();
    let active = settings::active_provider_with_key(&app);
    let key_state = match &active {
        Some((provider, Some(_))) => format!(
            "将注入 {}（provider: {}）",
            provider.api_key_env(),
            provider.id
        ),
        Some((provider, None)) => format!(
            "未配置 API Key（provider: {}），将基于 CLI 自身登录态运行",
            provider.id
        ),
        None => "还没选择模型提供商；运行时不会注入环境变量".to_string(),
    };

    Ok(vec![
        EngineInfo {
            kind: EngineKind::Claude,
            name: "Claude Code CLI".to_string(),
            available: claude.is_some(),
            detail: match &claude {
                Some(p) => format!("{}（{key_state}）", p.display()),
                None => "未检测到 claude；若 opencode 可用，任务会自动改走 opencode 引擎".to_string(),
            },
        },
        EngineInfo {
            kind: EngineKind::Opencode,
            name: "opencode CLI".to_string(),
            available: opencode.is_some(),
            detail: match &opencode {
                Some(p) => format!(
                    "{}（自定义 OpenAI 协议 API 走 opencode.json 映射；联网 webfetch/websearch 默认可用）",
                    p.display()
                ),
                None => "未检测到 opencode；点「下载 opencode」自动安装（自定义 API 推荐这条路）".to_string(),
            },
        },
    ])
}

/// 本程序自己的 opencode 数据目录用量（设置页用）。
///
/// 隔离之后会话库落在 `<app_local_data>/opencode/data/opencode/opencode.db`，
/// 和桌面版的全局库互不相干；这里只报自己的数字，并给出「该清理了」的提示。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenCodeStorage {
    pub root: String,
    pub db_bytes: u64,
    pub log_bytes: u64,
    pub warning: Option<String>,
}

#[tauri::command]
pub async fn opencode_storage(app: AppHandle) -> Result<OpenCodeStorage, StoryError> {
    let base = app
        .path()
        .app_local_data_dir()
        .map_err(|e| StoryError::internal(format!("取不到本程序的数据目录：{e}")))?;
    let dirs = crate::services::opencode_home::dirs_under(&base.join("opencode"));
    let _ = crate::services::opencode_home::ensure_dirs(&dirs);
    let db = dirs.db_path();
    let log = dirs.opencode_log();
    Ok(OpenCodeStorage {
        root: dirs.root.display().to_string(),
        db_bytes: std::fs::metadata(&db).map(|m| m.len()).unwrap_or(0),
        log_bytes: std::fs::metadata(&log).map(|m| m.len()).unwrap_or(0),
        warning: crate::services::opencode_home::db_warning(
            &db,
            crate::services::opencode_home::DB_WARN_BYTES,
        ),
    })
}

/// 当前 opencode 引擎的版本与来源，以及本地保留的 v1 回退件（第 8 项 C）。
///
/// 设置页要能回答三个问题：现在跑的是哪个版本、它是哪来的、能不能切回 v1。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpencodeEngineInfo {
    /// 当前生效的二进制路径（找不到为 None）
    pub path: Option<String>,
    /// 形如 `2.2.0`；探测失败为 None
    pub version: Option<String>,
    /// 「设置指定」/「PATH」/「本程序安装目录」/「未找到」
    pub source: String,
    /// 保留的 v1.19.5 回退件路径与版本
    pub fallback_path: Option<String>,
    pub fallback_version: Option<String>,
    /// 本程序钉死的下载版本
    pub pinned_version: String,
    /// 设置里指定的路径（可能写了但文件不存在）
    pub configured_path: Option<String>,
}

fn version_text(version: Option<(u32, u32)>) -> Option<String> {
    version.map(|(major, minor)| format!("{major}.{minor}"))
}

#[tauri::command]
pub async fn opencode_engine_info(app: AppHandle) -> Result<OpencodeEngineInfo, StoryError> {
    let configured = crate::services::settings::load(&app).opencode_path;
    let located = crate::services::opencode_setup::locate_preferring(configured.as_deref());

    let source = if let Some(path) = &located {
        let matches_configured = configured
            .as_deref()
            .map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(|raw| std::path::Path::new(raw) == path.as_path())
            .unwrap_or(false);
        if matches_configured {
            "设置指定"
        } else if crate::services::env_check::which_exe("opencode")
            .map(|found| &found == path)
            .unwrap_or(false)
        {
            "PATH"
        } else {
            "本程序安装目录"
        }
    } else {
        "未找到"
    };

    // 当前这条还是 v1 的话，顺手留一份回退件（只复制，不删不移）。
    let _ = crate::services::opencode_setup::ensure_fallback_copy();
    let fallback = crate::services::opencode_setup::fallback_path();
    let fallback_exists = fallback.is_file();

    Ok(OpencodeEngineInfo {
        version: located
            .as_deref()
            .and_then(crate::services::engine::probe_version)
            .map(|(major, minor)| format!("{major}.{minor}.x")),
        path: located.as_deref().map(|p| p.display().to_string()),
        source: source.to_string(),
        fallback_version: if fallback_exists {
            crate::services::engine::probe_version(&fallback)
                .map(|(major, minor)| format!("{major}.{minor}.x"))
        } else {
            None
        },
        fallback_path: if fallback_exists {
            Some(fallback.display().to_string())
        } else {
            None
        },
        pinned_version: crate::services::opencode_setup::PINNED_VERSION.to_string(),
        configured_path: configured,
    })
}

/// 自动下载并安装 opencode CLI（GitHub Releases → %LOCALAPPDATA%\storydesk\opencode\）。
///
/// 进度通过 `opencode://progress` 事件推送；已安装时直接返回现有路径。
#[tauri::command]
pub async fn engine_install_opencode(app: AppHandle) -> Result<String, StoryError> {
    crate::services::opencode_setup::install(&app).await
}

/// 自动下载运行时依赖（node / python / gitbash），国内镜像（npmmirror → 华为云兜底）。
///
/// 进度通过 `setup://progress` 事件推送（载荷带 `tool` 区分）；已安装直接返回路径。
#[tauri::command]
pub async fn runtime_install(tool: String, app: AppHandle) -> Result<String, StoryError> {
    let tool = crate::services::runtime_setup::RuntimeTool::parse(&tool).ok_or_else(|| {
        StoryError::internal(format!(
            "未知工具：{tool}（只支持 node / python / gitbash）"
        ))
    })?;
    crate::services::runtime_setup::install(tool, &app).await
}

// ---------------------------------------------------------------- 门禁

#[tauri::command]
pub async fn gate_probe(workspace_root: String) -> Result<GateInfo, StoryError> {
    // 以前直接 PathBuf::from 就把前端的字符串当成根用了：根不存在 / 不是目录 /
    // 指向别处时，gate::info 会在一个随便的目录上扫文件系统。统一走 path_guard。
    let ws = path_guard::validate_root(Path::new(&workspace_root))?;
    crate::commands::blocking(move || Ok(gate::info(&ws))).await
}

#[tauri::command]
pub async fn gate_chapter_check(
    workspace_root: String,
    book_root: String,
    chapter: u32,
) -> Result<GateOutput, StoryError> {
    let (ws, book) = resolve_pair(&workspace_root, &book_root)?;
    crate::commands::blocking(move || gate::chapter_check(&ws, &book, chapter)).await
}

#[tauri::command]
pub async fn gate_postcheck(
    workspace_root: String,
    book_root: String,
    chapter: u32,
) -> Result<PostCheck, StoryError> {
    let (ws, book) = resolve_pair(&workspace_root, &book_root)?;
    let mut post = crate::commands::blocking({
        let ws = ws.clone();
        let book = book.clone();
        move || Ok(gate::postcheck_prose(&ws, &book, chapter, "manual"))
    })
    .await?;
    // 手动重体检也顺手清一遍正文末尾的创作注释：用户在体检卡片上看到「已搬到…」为止。
    match crate::commands::blocking(move || memory::detach_writer_notes(&ws, &book, chapter)).await
    {
        Ok(Some(report)) => {
            post.stdout = join_line(
                &post.stdout,
                &format!(
                    "[正文清理] 第{}章的创作注释（{} 字）已搬到 {}，正文只留故事。",
                    report.chapter, report.moved_chars, report.notes_rel
                ),
            );
        }
        Ok(None) => {}
        Err(e) => {
            post.stdout = join_line(&post.stdout, &format!("[正文清理] 没搬成：{e}"));
        }
    }
    Ok(post)
}

#[tauri::command]
pub async fn gate_build_packet(
    workspace_root: String,
    book_root: String,
    task: String,
    volume: Option<u32>,
    chapters: Option<String>,
) -> Result<TaskPacket, StoryError> {
    let (ws, book) = resolve_pair(&workspace_root, &book_root)?;
    let range = chapters.as_deref().and_then(|raw| {
        let (a, b) = raw.split_once('-')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    });
    crate::commands::blocking(move || gate::build_task_packet(&ws, &book, &task, volume, range))
        .await
}

// ---------------------------------------------------------------- 密钥
// 阶段 5 起 API Key 按「提供商」分别存放在系统钥匙串，命令搬到 commands/settings.rs：
//   set_provider_key / check_provider_key / delete_provider_key。
// 这里不再保留全局 key 命令——key 永远不回前端，前端只拿到 hasKey 布尔。

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_matches_contract() {
        let prompt = build_prompt(
            Path::new("D:/桌面/大制作/小说/验收示例/长篇/我的小说"),
            Some(Path::new("D:/x/.storydesk/prompts/outline-001.md")),
            None,
        );
        let lines: Vec<&str> = prompt.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("项目目录："));
        assert!(lines[1].starts_with("任务包："));
        assert_eq!(
            lines[2],
            "先完整读取任务包，按包里的流程与模板完成；作者已定的方向、设定和要求都在 设定/，对话内容不会传给你"
        );
        assert_eq!(lines[3], "交付后只回任务包开头要求的几项");
    }

    #[test]
    fn agent_prompt_keeps_the_four_line_contract() {
        let agent = AgentConfig::from_spec(ai::spec(AgentRole::Settings));
        let files = vec![
            "设定/文风.md".to_string(),
            "设定/世界观/太虚.md".to_string(),
        ];
        let contract = build_prompt(Path::new("D:/book"), None, None);
        let tools = ai::default_tools();
        let global = vec![
            "大纲/细纲_第002章.md".to_string(),
            "设定/角色状态.md".to_string(),
        ];
        let prompt = engine::compose_agent_prompt(
            &agent, &tools, &files, &global, false, None, &contract, None, None,
        );

        assert!(prompt.starts_with("你现在是「野菜工作台」的【设定管理员】。"));
        assert!(prompt.contains("你的核心职责是处理 设定/ 目录下的内容。"));
        assert!(prompt.contains("角色约束："));
        assert!(prompt.contains("共 2 个"));
        assert!(prompt.contains("- 设定/文风.md"));
        // 阶段 9：工具清单来自设置，默认只有 Read / Write
        assert!(prompt.contains("允许的工具：Read、Write。"));
        // 阶段 10：全局必读清单排在目标目录清单之前
        assert!(prompt.contains("动笔前，必须先阅读以下全局设定与大纲要求"));
        assert!(prompt.contains("- 大纲/细纲_第002章.md"));
        assert!(
            prompt.find("动笔前").unwrap() < prompt.find("目标目录 设定/").unwrap(),
            "全局必读要写在目标目录清单前面"
        );
        // 契约原样追加在末尾：四行一个字都没变
        assert_eq!(contract.lines().count(), 4);
        assert!(prompt.ends_with(&contract), "四行契约必须原样保留在末尾");

        // 目录空的时候要说清楚，不能留个空标题
        let empty = engine::compose_agent_prompt(
            &agent,
            &tools,
            &[],
            &[],
            false,
            None,
            &contract,
            None,
            None,
        );
        assert!(empty.contains("（还没有文件）"));
        assert!(empty.ends_with(&contract));
    }

    #[test]
    fn read_only_prompt_says_no_write() {
        // 阶段 10 验收 2 的最小复现：只读任务的 prompt 第一行就要写死「不要调用 Write」
        let agent = AgentConfig::from_spec(ai::spec(AgentRole::Tracker));
        let contract = build_prompt(
            Path::new("D:/book"),
            None,
            Some("【只读模式】检查伏笔与时间线"),
        );
        let tools = ai::read_only_tools(&ai::default_tools());
        assert_eq!(tools, vec!["Read".to_string()], "只读任务必须摘掉 Write");
        let prompt = engine::compose_agent_prompt(
            &agent,
            &tools,
            &[],
            &[],
            true,
            None,
            &contract,
            None,
            None,
        );
        assert!(prompt.contains("【只读模式】不要调用 Write 工具，只输出报告"));
        assert!(prompt.contains("允许的工具：Read。"));
        assert!(
            !prompt.contains("、Write"),
            "只读 prompt 里不该再出现 Write 工具"
        );
        assert!(prompt.ends_with(&contract));
    }

    #[test]
    fn unchecked_tool_disappears_from_prompt() {
        // 验收 2 的最小复现：设置里取消勾选 Write → prompt 里不再出现 Write
        let agent = AgentConfig::from_spec(ai::spec(AgentRole::Writer));
        let contract = build_prompt(Path::new("D:/book"), None, None);
        let prompt = engine::compose_agent_prompt(
            &agent,
            &["Read".to_string()],
            &[],
            &[],
            false,
            None,
            &contract,
            None,
            None,
        );
        assert!(prompt.contains("允许的工具：Read。"));
        assert!(
            !prompt.contains("Write"),
            "取消勾选的 Write 不能还留在 prompt 里"
        );

        // 一个都没勾：明说「只能输出文字说明」，而不是留一行空的「允许的工具：」
        let none =
            engine::compose_agent_prompt(&agent, &[], &[], &[], false, None, &contract, None, None);
        assert!(none.contains("一个都没勾选"));
        assert!(none.ends_with(&contract));
    }

    #[test]
    fn unknown_target_dir_falls_back_to_the_agent_dir() {
        let agent = AgentConfig::from_spec(ai::spec(AgentRole::Writer));
        assert_eq!(pick_dir(&agent, Some("设定")), "设定");
        assert_eq!(pick_dir(&agent, Some(" 正文/ ")), "正文");
        assert_eq!(pick_dir(&agent, Some("../../etc")), "正文");
        assert_eq!(pick_dir(&agent, Some("乱写")), "正文");
        assert_eq!(pick_dir(&agent, None), "正文");
    }

    #[test]
    fn custom_system_prompt_reaches_the_prompt() {
        // 验收 1 的最小复现：改提示词 → 下一份 prompt 里立刻是新内容
        let mut agent = AgentConfig::from_spec(ai::spec(AgentRole::Architect));
        agent.system_prompt = "每章先写一句「本章卖点」，再拆三段式。".to_string();
        let prompt = engine::compose_agent_prompt(
            &agent,
            &[],
            &[],
            &[],
            false,
            None,
            "项目目录：/x",
            None,
            None,
        );
        assert!(prompt.contains("角色约束：每章先写一句「本章卖点」，再拆三段式。"));
        assert!(prompt.ends_with("项目目录：/x"));
    }

    #[test]
    fn read_only_detection_splits_checks_from_writes() {
        // 检查类：只读
        assert!(ai::is_read_only_task("检查伏笔与时间线"));
        assert!(ai::is_read_only_task("核对人物设定有没有冲突"));
        assert!(ai::is_read_only_task("【只读模式】随便看看"));
        // 生成类：哪怕句子里带「检查」也不算只读
        assert!(!ai::is_read_only_task("写本章正文并检查错别字"));
        assert!(!ai::is_read_only_task("生成第 2 章细纲：目标、冲突"));
        // 主 AI 亲手标了【只读模式】就以它为准（哪怕句子里还有「写」字）
        assert!(ai::is_read_only_task("【只读模式】别写文件，只列问题"));
    }

    #[test]
    fn next_chapter_starts_at_one() {
        let tmp = std::env::temp_dir().join("storydesk-ai-test-book");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("正文")).unwrap();
        assert_eq!(next_chapter_no(&tmp), 1);
        std::fs::write(tmp.join("正文").join("第007章_x.md"), "x").unwrap();
        assert_eq!(next_chapter_no(&tmp), 8);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 章号解析：个位数与多位数按数值认，不按字符串。
    #[test]
    fn chapter_numbers_are_read_numerically() {
        assert_eq!(chapter_of_name("第9章_夜谈.md"), Some(9));
        assert_eq!(chapter_of_name("第10章_夜谈.md"), Some(10));
        assert_eq!(chapter_of_name("细纲_第012章.md"), Some(12));
        assert_eq!(chapter_of_name("大纲.md"), None);
        assert_eq!(chapter_of_name("第0章_引子.md"), None);
        // 区间式文件名认不出单一章号就该返回 None（宁可当「无章号」也不猜）。
        assert_eq!(chapter_of_name("卷2_第005-012章.md"), None);
        // 数值序：第 9 章必须排在 第 10 章 前面。
        let mut names = vec!["第10章.md", "第9章.md", "第100章.md"];
        names.sort_by_key(|n| chapter_of_name(n));
        assert_eq!(names, vec!["第9章.md", "第10章.md", "第100章.md"]);
    }

    /// 300 多个大纲文件时，必须挑当前章附近的，而不是字典序前 40 个。
    #[test]
    fn outline_context_follows_the_current_chapter() {
        let tmp = std::env::temp_dir().join("storydesk-ai-test-outline");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("大纲")).unwrap();
        std::fs::write(tmp.join("大纲").join("大纲.md"), "# 全书大纲\n").unwrap();
        for ch in 1..=60u32 {
            std::fs::write(
                tmp.join("大纲").join(format!("卷_第{ch:03}章.md")),
                "# 细纲\n",
            )
            .unwrap();
        }
        let near = collect_context_files_near(&tmp, "大纲", &[], Some(58));
        assert_eq!(near.len(), engine::CONTEXT_LIMIT, "取满上限");
        assert_eq!(
            near.first().map(String::as_str),
            Some("大纲/卷_第058章.md"),
            "当前章必须排第一：{near:?}"
        );
        assert!(
            near.iter().any(|p| p == "大纲/卷_第059章.md"),
            "相邻章要在里面：{near:?}"
        );
        assert!(
            !near.iter().any(|p| p == "大纲/卷_第001章.md"),
            "写到第 58 章时不该只看到开头：{near:?}"
        );
        // 认不出章号时：全书级文件排第一，其余保持字典序（安全回退）。
        let none = collect_context_files_near(&tmp, "大纲", &[], None);
        assert_eq!(
            none.first().map(String::as_str),
            Some("大纲/大纲.md"),
            "没有当前章时全书大纲不能漏：{none:?}"
        );
        let rest: Vec<String> = none[1..].to_vec();
        let mut expected = rest.clone();
        expected.sort();
        assert_eq!(rest, expected, "其余保持字典序");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// 前端传清单时也要过一遍：不存在的路径不许进 prompt，当前章仍排第一，
    /// 整份都用不上时回落到扫目录（不能返回空清单让模型裸写）。
    #[test]
    fn given_context_files_are_validated_and_ordered() {
        let tmp = std::env::temp_dir().join("storydesk-ai-test-given");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("大纲")).unwrap();
        for ch in 1..=8u32 {
            std::fs::write(tmp.join("大纲").join(format!("细纲_第{ch:03}章.md")), "x").unwrap();
        }
        let given = vec![
            "大纲/细纲_第001章.md".to_string(),
            "大纲/已经删掉的.md".to_string(),
            "大纲/细纲_第007章.md".to_string(),
            "大纲/细纲_第001章.md".to_string(),
            "设定/角色.md".to_string(),
        ];
        let out = collect_context_files_near(&tmp, "大纲", &given, Some(7));
        assert_eq!(
            out.first().map(String::as_str),
            Some("大纲/细纲_第007章.md"),
            "前端清单同样按章节距离排：{out:?}"
        );
        assert!(
            out.iter().all(|p| !p.contains("已经删掉")),
            "不存在的文件要忽略：{out:?}"
        );
        assert_eq!(out.len(), 2, "重复项去重后只剩两份：{out:?}");

        let bad = vec!["大纲/没有这个.md".to_string()];
        let fallback = collect_context_files_near(&tmp, "大纲", &bad, Some(7));
        assert_eq!(fallback.len(), 8, "应当回落到扫目录：{fallback:?}");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
