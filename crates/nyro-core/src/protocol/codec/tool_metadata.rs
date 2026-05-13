use std::collections::HashMap;

use serde_json::{Value, json};

pub const GOOGLE_TOOL_CALL_METADATA_KEY: &str = "__google_tool_call_metadata";

pub fn append_google_tool_call_metadata(
    extra: &mut HashMap<String, Value>,
    index: usize,
    id: &str,
    name: &str,
    thought_signature: Option<&str>,
) {
    let Some(thought_signature) = thought_signature
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return;
    };

    let entry = json!({
        "index": index,
        "id": id,
        "name": name,
        "thought_signature": thought_signature,
    });

    match extra.get_mut(GOOGLE_TOOL_CALL_METADATA_KEY) {
        Some(Value::Array(entries)) => entries.push(entry),
        _ => {
            extra.insert(
                GOOGLE_TOOL_CALL_METADATA_KEY.to_string(),
                Value::Array(vec![entry]),
            );
        }
    }
}

pub fn lookup_google_tool_call_thought_signature(
    extra: &HashMap<String, Value>,
    index: Option<usize>,
    id: Option<&str>,
    name: Option<&str>,
) -> Option<String> {
    let entries = extra
        .get(GOOGLE_TOOL_CALL_METADATA_KEY)
        .and_then(|value| value.as_array())?;

    lookup_by_id(entries, id)
        .or_else(|| lookup_by_index(entries, index))
        .or_else(|| lookup_by_name(entries, name))
}

fn lookup_by_id(entries: &[Value], id: Option<&str>) -> Option<String> {
    let id = id?.trim();
    if id.is_empty() {
        return None;
    }

    entries.iter().find_map(|entry| {
        let matches = entry.get("id").and_then(|value| value.as_str()) == Some(id);
        matches
            .then(|| thought_signature_from_entry(entry))
            .flatten()
    })
}

fn lookup_by_index(entries: &[Value], index: Option<usize>) -> Option<String> {
    let index = index? as u64;
    entries.iter().find_map(|entry| {
        let matches = entry.get("index").and_then(|value| value.as_u64()) == Some(index);
        matches
            .then(|| thought_signature_from_entry(entry))
            .flatten()
    })
}

fn lookup_by_name(entries: &[Value], name: Option<&str>) -> Option<String> {
    let name = name?.trim();
    if name.is_empty() {
        return None;
    }

    entries.iter().find_map(|entry| {
        let matches = entry.get("name").and_then(|value| value.as_str()) == Some(name);
        matches
            .then(|| thought_signature_from_entry(entry))
            .flatten()
    })
}

fn thought_signature_from_entry(entry: &Value) -> Option<String> {
    entry
        .get("thought_signature")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
