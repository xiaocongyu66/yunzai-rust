//! ≈ lib/modules/oicq/index.js — segment 消息段构造器（oicq 扁平格式）
use serde_json::{json, Value};

pub fn custom(t: &str, mut data: Value) -> Value {
    if let Some(obj) = data.as_object_mut() {
        obj.insert("type".into(), json!(t));
    }
    data
}

pub fn raw(data: Value) -> Value {
    json!({ "type": "raw", "data": data })
}

pub fn button(data: Value) -> Value {
    json!({ "type": "button", "data": data })
}

pub fn markdown(data: Value) -> Value {
    json!({ "type": "markdown", "data": data })
}

pub fn text(t: impl Into<Value>) -> Value {
    json!({ "type": "text", "text": t.into() })
}

pub fn image(file: impl Into<Value>, name: Option<Value>) -> Value {
    let mut o = json!({ "type": "image", "file": file.into() });
    if let Some(n) = name {
        o["name"] = n;
    }
    o
}

pub fn at(qq: impl Into<Value>, name: Option<Value>) -> Value {
    let mut o = json!({ "type": "at", "qq": qq.into() });
    if let Some(n) = name {
        o["name"] = n;
    }
    o
}

pub fn record(file: impl Into<Value>, name: Option<Value>) -> Value {
    let mut o = json!({ "type": "record", "file": file.into() });
    if let Some(n) = name {
        o["name"] = n;
    }
    o
}

pub fn video(file: impl Into<Value>, name: Option<Value>) -> Value {
    let mut o = json!({ "type": "video", "file": file.into() });
    if let Some(n) = name {
        o["name"] = n;
    }
    o
}

pub fn file(file: impl Into<Value>, name: Option<Value>) -> Value {
    let mut o = json!({ "type": "file", "file": file.into() });
    if let Some(n) = name {
        o["name"] = n;
    }
    o
}

pub fn reply(id: impl Into<Value>) -> Value {
    json!({ "type": "reply", "id": id.into() })
}

pub fn node(data: Value) -> Value {
    json!({ "type": "node", "data": data })
}

/// 消息归一为段数组：字符串 → text 段
pub fn to_array(msg: &Value) -> Vec<Value> {
    match msg {
        Value::Array(a) => a.clone(),
        Value::String(s) => vec![text(s.clone())],
        Value::Object(_) => vec![msg.clone()],
        _ => vec![text(stringify(msg))],
    }
}

pub fn stringify(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(v).unwrap_or_default(),
    }
}
