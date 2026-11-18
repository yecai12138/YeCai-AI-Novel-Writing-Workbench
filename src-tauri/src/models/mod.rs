//! 数据模型：前端 DTO 与统一错误类型。
//!
//! 所有跨 IPC 的结构字段名都是单词语义，序列化后保持原样（camelCase 只作用于结构体名下的多词字段，
//! 这里刻意不引入字段重命名，前端 TS 类型可逐字段对应）。

pub mod ai;
pub mod chat;
pub mod dto;
pub mod error;
pub mod event;
pub mod settings;

pub use error::StoryError;
