//! ≈ lib/plugins/ — 插件系统
pub mod builtin;
pub mod handler;
pub mod loader;
pub mod plugin;

pub use plugin::{CtxEntry, Permission};
