//! Builds CDP `RemoteObject` shapes from plain `serde_json::Value`s.
//!
//! Shared by `domain_dom` (`DOM.resolveNode`) and `domain_page`
//! (`Runtime.callFunctionOn`) so both answer with the same shape instead of
//! two independently-guessed ones. Deliberately minimal for Phase 0: no
//! lazy-expansion object registry (`Runtime.getProperties` on a returned
//! `objectId` isn't implemented — the inline `preview` is what the frontend
//! shows, and it needs nothing else to render correctly). Add the registry
//! in a later phase only if something is found that actually needs deeper
//! expansion than the preview gives.

use serde_json::{json, Value};

/// A `RemoteObject` for any JSON value. Primitives are sent by value with an
/// explicit `description` (grid cells render blank without one — this was a
/// real bug hit in the Swift reference implementation, not a hypothetical).
pub fn remote_object(value: &Value) -> Value {
    match value {
        Value::Null => json!({ "type": "object", "subtype": "null", "value": Value::Null }),
        Value::Bool(b) => json!({ "type": "boolean", "value": b, "description": b.to_string() }),
        Value::Number(n) => json!({ "type": "number", "value": n, "description": n.to_string() }),
        Value::String(s) => json!({ "type": "string", "value": s, "description": s }),
        Value::Array(items) => json!({
            "type": "object",
            "subtype": "array",
            "className": "Array",
            "description": format!("Array({})", items.len()),
            "preview": preview(value),
        }),
        Value::Object(_) => json!({
            "type": "object",
            "className": "Object",
            "description": "Object",
            "preview": preview(value),
        }),
    }
}

/// `ObjectPreview` for a container. Nested containers stay short
/// descriptions per spec — only the top level's properties are listed.
fn preview(value: &Value) -> Value {
    const MAX_PROPERTIES: usize = 10;
    match value {
        Value::Array(items) => json!({
            "type": "object",
            "subtype": "array",
            "description": format!("Array({})", items.len()),
            "overflow": items.len() > MAX_PROPERTIES,
            "properties": items.iter().take(MAX_PROPERTIES).enumerate()
                .map(|(i, v)| preview_property(&i.to_string(), v))
                .collect::<Vec<_>>(),
        }),
        Value::Object(map) => json!({
            "type": "object",
            "description": "Object",
            "overflow": map.len() > MAX_PROPERTIES,
            "properties": map.iter().take(MAX_PROPERTIES)
                .map(|(k, v)| preview_property(k, v))
                .collect::<Vec<_>>(),
        }),
        _ => {
            json!({ "type": "object", "description": "Object", "overflow": false, "properties": [] })
        }
    }
}

/// `PropertyPreview.value` is always a string per the CDP spec; nested
/// containers get a short description instead of their contents.
fn preview_property(name: &str, value: &Value) -> Value {
    let (kind, display) = match value {
        Value::Bool(b) => ("boolean", b.to_string()),
        Value::Number(n) => ("number", n.to_string()),
        Value::String(s) => ("string", s.clone()),
        Value::Array(items) => ("object", format!("Array({})", items.len())),
        Value::Object(_) => ("object", "Object".to_string()),
        Value::Null => ("object", "null".to_string()),
    };
    json!({ "name": name, "type": kind, "value": display })
}
