//! 编排运行状态：`<书>/.storydesk/runs/<run_id>.json`，让「第 N 步失败整份计划作废」变成可续跑。
//!
//! 三条约束（与 [`crate::services::chat_store`] 同款）：
//! 1. **run_id 只允许 `[a-z0-9_-]`**（≤64 字符）——它是文件名，不校验就能 `../` 逃逸。
//! 2. **整份重写 + 原子写**（[`crate::services::atomic_write`]）：几十步的状态几十 KB，
//!    整份重写比 append 安全，断电不会留半截 JSON。
//! 3. **读坏了就当没有**：手改坏 JSON 只该让「续跑」不可用，不该让派发报错。
//!
//! 状态文件只记「跑到哪、哪步成了、失败原因是什么」，不代替聊天记录：
//! 前端仍然从 `chats/<session>.json` 渲染卡片，这里只服务断点续跑与失败重试。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::models::dto::{MasterPlan, PlanStep};
use crate::models::StoryError;
use crate::services::atomic_write;

/// 运行状态目录（相对书根）。
pub const RUN_DIR_REL: &str = ".storydesk/runs";

/// 一步的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepState {
    pub index: usize,
    #[serde(default)]
    pub agent_id: String,
    #[serde(default)]
    pub task: String,
    /// pending | running | done | failed | blocked | cancelled | skipped
    #[serde(default = "default_status")]
    pub status: String,
    /// 引擎重试次数（含首次；1 = 一次就成）
    #[serde(default)]
    pub attempts: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// 这一步落盘的文件（相对书根）
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub finished_at: Option<i64>,
}

fn default_status() -> String {
    "pending".to_string()
}

impl StepState {
    fn new(index: usize, step: &PlanStep) -> Self {
        Self {
            index,
            agent_id: step.agent_id.clone(),
            task: step.task.clone(),
            status: default_status(),
            attempts: 0,
            last_error: None,
            artifacts: Vec::new(),
            started_at: None,
            finished_at: None,
        }
    }

    pub fn is_done(&self) -> bool {
        self.status == "done" || self.status == "skipped"
    }
}

/// 一轮编排的状态。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunState {
    pub run_id: String,
    /// 用户原话（续跑时重新派发要用，不能让用户再打一遍）
    #[serde(default)]
    pub request: String,
    /// llm | rules | confirmed（沿用规划那一轮的标记）
    #[serde(default)]
    pub source: Option<String>,
    pub plan: MasterPlan,
    #[serde(default)]
    pub steps: Vec<StepState>,
    /// 当前游标（最后一个开始执行的下标），只用于诊断
    #[serde(default)]
    pub cursor: usize,
    #[serde(default)]
    pub updated_at: i64,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn valid_run_id(run_id: &str) -> bool {
    let id = run_id.trim();
    !id.is_empty()
        && id.len() <= 64
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

/// `<book_root>/.storydesk/runs`。
pub fn runs_dir(book_root: &Path) -> PathBuf {
    book_root.join(RUN_DIR_REL)
}

/// `<book_root>/.storydesk/runs/<run_id>.json`（校验过 id）。
pub fn run_path(book_root: &Path, run_id: &str) -> Result<PathBuf, StoryError> {
    if !valid_run_id(run_id) {
        return Err(StoryError::invalid(
            "runId",
            "运行 id 只能用小写字母、数字、- 和 _（最长 64 字符）",
        ));
    }
    Ok(runs_dir(book_root).join(format!("{}.json", run_id.trim())))
}

/// 读状态；文件不存在或读坏了都返回 None（续跑不可用，但派发照常）。
pub fn load(book_root: &Path, run_id: &str) -> Option<RunState> {
    let path = run_path(book_root, run_id).ok()?;
    let raw = fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<RunState>(&raw) {
        Ok(state) => Some(state),
        Err(e) => {
            eprintln!(
                "[storydesk] 运行状态读坏了，按全新一轮处理：{}（{e}）",
                path.display()
            );
            None
        }
    }
}

/// 开一轮（已有同一 run_id 且步数对得上就接着用它，否则重开）。
pub fn begin(
    book_root: &Path,
    run_id: &str,
    request: &str,
    source: Option<&str>,
    plan: &MasterPlan,
) -> RunState {
    if let Some(mut old) = load(book_root, run_id) {
        if old.plan.steps.len() == plan.steps.len() {
            old.request = request.to_string();
            old.source = source.map(|s| s.to_string()).or(old.source);
            // 崩溃残留的 running 一律退回 pending：那个进程已经没了，没人会替它收尾。
            for step in old.steps.iter_mut() {
                if step.status == "running" {
                    step.status = default_status();
                }
            }
            return old;
        }
    }
    RunState {
        run_id: run_id.to_string(),
        request: request.to_string(),
        source: source.map(|s| s.to_string()),
        plan: plan.clone(),
        steps: plan
            .steps
            .iter()
            .enumerate()
            .map(|(i, step)| StepState::new(i, step))
            .collect(),
        cursor: 0,
        updated_at: now_secs(),
    }
}

impl RunState {
    /// 步数变了（拆步追加了续写步）：补齐状态条目，并把最新的步骤定义写回 plan。
    pub fn sync(&mut self, steps: &[PlanStep]) {
        while self.steps.len() < steps.len() {
            let index = self.steps.len();
            self.steps.push(StepState::new(index, &steps[index]));
        }
        self.plan.steps = steps.to_vec();
    }

    pub fn status_of(&self, index: usize) -> &str {
        self.steps
            .get(index)
            .map(|s| s.status.as_str())
            .unwrap_or("pending")
    }

    pub fn is_done(&self, index: usize) -> bool {
        self.steps.get(index).map(|s| s.is_done()).unwrap_or(false)
    }

    /// 第一个没完成的步（`None` = 全跑完了）。
    pub fn first_unfinished(&self) -> Option<usize> {
        self.steps.iter().position(|s| !s.is_done())
    }

    pub fn done_count(&self) -> usize {
        self.steps.iter().filter(|s| s.is_done()).count()
    }

    /// 记一次状态变化（内存里，不落盘；落盘走 [`RunState::write`]）。
    pub fn set(&mut self, index: usize, status: &str, attempts: u32, error: Option<String>) {
        while self.steps.len() <= index {
            let fill = self.steps.len();
            let step = self.plan.steps.get(fill).cloned().unwrap_or_default();
            self.steps.push(StepState::new(fill, &step));
        }
        let now = now_secs();
        if let Some(step) = self.steps.get_mut(index) {
            step.status = status.to_string();
            step.attempts = attempts;
            step.last_error = error;
            if status == "running" {
                step.started_at = Some(now);
            } else if status != "pending" {
                step.finished_at = Some(now);
            }
        }
        self.cursor = index;
        self.updated_at = now;
    }

    /// 在中间插一步（门禁可修复时插入补步）：状态条目跟着平移，下标始终与 `steps` 对齐。
    pub fn insert_step(&mut self, index: usize, step: &PlanStep) {
        let index = index.min(self.steps.len());
        self.steps.insert(index, StepState::new(index, step));
        for (i, state) in self.steps.iter_mut().enumerate() {
            state.index = i;
        }
        self.updated_at = now_secs();
    }

    /// 记这一步落了哪些文件。
    pub fn set_artifacts(&mut self, index: usize, artifacts: Vec<String>) {
        if let Some(step) = self.steps.get_mut(index) {
            step.artifacts = artifacts;
        }
    }

    /// 落盘（整份重写 + 原子写）。
    pub fn write(&mut self, book_root: &Path) -> Result<(), StoryError> {
        let path = run_path(book_root, &self.run_id)?;
        self.updated_at = now_secs();
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| StoryError::invalid("runState", format!("运行状态序列化失败：{e}")))?;
        atomic_write::write_atomic(&path, json.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::dto::MasterPlan;

    fn plan_of(n: usize) -> MasterPlan {
        MasterPlan {
            source: "rules".to_string(),
            intent: None,
            steps: (1..=n)
                .map(|i| PlanStep {
                    agent_id: "writer".to_string(),
                    task: format!("写第 {i} 章"),
                    ..Default::default()
                })
                .collect(),
            raw: String::new(),
        }
    }

    fn tmp_book(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("storydesk-runstate-{tag}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn run_state_round_trips_and_skips_done_steps() {
        let book = tmp_book("roundtrip");
        let plan = plan_of(3);
        let mut state = begin(&book, "run-1", "写三章", Some("rules"), &plan);
        assert_eq!(state.first_unfinished(), Some(0));
        state.set(0, "done", 1, None);
        state.set(1, "running", 1, None);
        state.write(&book).unwrap();

        // 读回来：done 留在原位，running（进程没了）退回 pending，续跑从第 2 步开始。
        let mut reloaded = begin(&book, "run-1", "写三章", Some("rules"), &plan);
        assert!(reloaded.is_done(0));
        assert_eq!(reloaded.status_of(1), "pending");
        assert_eq!(reloaded.first_unfinished(), Some(1));
        assert_eq!(reloaded.done_count(), 1);

        reloaded.set(1, "done", 1, None);
        reloaded.set(2, "done", 1, None);
        reloaded.write(&book).unwrap();
        assert_eq!(reloaded.first_unfinished(), None);
        let _ = fs::remove_dir_all(&book);
    }

    #[test]
    fn different_step_count_starts_over() {
        let book = tmp_book("reset");
        let mut state = begin(&book, "run-2", "写两章", None, &plan_of(2));
        state.set(0, "done", 1, None);
        state.write(&book).unwrap();

        // 用户改了口径、计划从 2 步变 4 步：旧状态不再适用，必须从头开始。
        let fresh = begin(&book, "run-2", "写四章", None, &plan_of(4));
        assert_eq!(fresh.steps.len(), 4);
        assert!(fresh.steps.iter().all(|s| s.status == "pending"));
        let _ = fs::remove_dir_all(&book);
    }

    #[test]
    fn sync_appends_states_for_retry_steps() {
        let book = tmp_book("sync");
        let mut plan = plan_of(1);
        let mut state = begin(&book, "run-3", "写一章", None, &plan);
        state.set(0, "done", 1, None);
        // 编排器在末尾追加了一条续写步：状态要跟着长出来，plan 也要跟着更新。
        plan.steps.push(PlanStep {
            agent_id: "writer".to_string(),
            task: "续写第 1 章".to_string(),
            ..Default::default()
        });
        state.sync(&plan.steps);
        assert_eq!(state.steps.len(), 2);
        assert_eq!(state.status_of(1), "pending");
        assert_eq!(state.plan.steps.len(), 2);
        assert_eq!(state.first_unfinished(), Some(1));
        let _ = fs::remove_dir_all(&book);
    }

    #[test]
    fn bad_run_id_is_rejected() {
        let book = tmp_book("badid");
        assert!(run_path(&book, "../escape").is_err());
        assert!(run_path(&book, "run-1").is_ok());
        let _ = fs::remove_dir_all(&book);
    }
}
