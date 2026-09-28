//! ≈ plugins/adapter/OneBotv11.js — 阶段2实现，本文件先注册适配器元信息占位
use crate::bot::AdapterMeta;

pub fn meta() -> AdapterMeta {
    AdapterMeta { id: "QQ".into(), name: "OneBotv11".into(), path: "OneBotv11".into() }
}
