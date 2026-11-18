use serde::Serialize;

/// 统一错误类型：序列化后形如 `{ "kind": "fs", "message": "...", "path": "..." }`。
///
/// 前端按 `kind` 分支展示，绝不依赖 message 文本做逻辑判断。
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind")]
pub enum StoryError {
    /// 缺少运行环境依赖（node / python / bash / 宿主 Agent CLI）。
    #[serde(rename = "envMissing")]
    EnvMissing {
        tool: String,
        message: String,
        hint: String,
    },
    /// 文件系统错误。
    #[serde(rename = "fs")]
    Fs { message: String, path: String },
    /// 目标路径逃出了工作区根（路径穿越 / 符号链接逃逸）。
    #[serde(rename = "pathEscape")]
    PathEscape { target: String, root: String },
    /// 入参不合法。
    #[serde(rename = "invalidInput")]
    InvalidInput { field: String, message: String },
    /// 其它内部错误（线程 join 失败等）。
    #[serde(rename = "internal")]
    Internal { message: String },
}

impl StoryError {
    pub fn fs(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self::Fs {
            message: message.into(),
            path: path.into(),
        }
    }

    pub fn invalid(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self::InvalidInput {
            field: field.into(),
            message: message.into(),
        }
    }

    pub fn env_missing(
        tool: impl Into<String>,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self::EnvMissing {
            tool: tool.into(),
            message: message.into(),
            hint: hint.into(),
        }
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::Internal {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EnvMissing { tool, message, .. } => write!(f, "[环境缺失:{tool}] {message}"),
            Self::Fs { message, path } => write!(f, "[文件系统] {path}: {message}"),
            Self::PathEscape { target, root } => {
                write!(f, "[路径越界] {target} 不在工作区 {root} 内")
            }
            Self::InvalidInput { field, message } => write!(f, "[参数不合法:{field}] {message}"),
            Self::Internal { message } => write!(f, "[内部错误] {message}"),
        }
    }
}

impl std::error::Error for StoryError {}
