use anyhow::Result;
use reqwest::header::HeaderMap;
use serde_json::Value;

use crate::protocol::EgressEncoder;
use crate::protocol::codec::tool_metadata::lookup_google_tool_call_thought_signature;
use crate::protocol::types::*;

pub struct GoogleEncoder;

impl EgressEncoder for GoogleEncoder {
    fn encode_request(&self, req: &InternalRequest) -> Result<(Value, HeaderMap)> {
        // ── System instruction ────────────────────────────────────────────────
        let system_val: Option<Value> =
            if let Some(v) = req.extra.get("__google_raw_system_instruction") {
                Some(v.clone())
            } else {
                let mut system_parts: Vec<Value> = Vec::new();
                for msg in &req.messages {
                    if msg.role == Role::System {
                        system_parts.push(serde_json::json!({"text": msg.content.as_text()}));
                    }
                }
                if system_parts.is_empty() {
                    None
                } else {
                    Some(serde_json::json!({"parts": system_parts}))
                }
            };

        // ── Contents ─────────────────────────────────────────────────────────
        let mut contents: Vec<Value> = Vec::new();
        for msg in &req.messages {
            if msg.role == Role::System {
                continue;
            }
            contents.push(encode_content(msg)?);
        }

        let mut body = serde_json::json!({ "contents": contents });
        let obj = body.as_object_mut().unwrap();

        if let Some(sv) = system_val {
            obj.insert("systemInstruction".into(), sv);
        }

        // ── generationConfig ──────────────────────────────────────────────────
        // Start from extra (full preserved config) and layer InternalRequest
        // overrides on top so model-override and routing changes still apply.
        let mut gen_config: serde_json::Map<String, Value> =
            if let Some(Value::Object(m)) = req.extra.get("__google_generation_config") {
                m.clone()
            } else {
                serde_json::Map::new()
            };

        if let Some(t) = req.temperature {
            gen_config.insert("temperature".into(), t.into());
        }
        if let Some(m) = req.max_tokens {
            gen_config.insert("maxOutputTokens".into(), m.into());
        }
        if let Some(p) = req.top_p {
            gen_config.insert("topP".into(), p.into());
        }

        if !gen_config.is_empty() {
            obj.insert("generationConfig".into(), Value::Object(gen_config));
        }

        // ── Tools ─────────────────────────────────────────────────────────────
        // Prefer raw tools (preserves built-ins) if present.
        if let Some(raw) = req.extra.get("__google_raw_tools") {
            obj.insert("tools".into(), raw.clone());
        } else if let Some(ref tools) = req.tools {
            let mut fn_decls: Vec<Value> = Vec::new();
            let mut builtin_entries: Vec<Value> = Vec::new();

            for t in tools {
                match t.name.as_str() {
                    "__builtin__google_search" => {
                        builtin_entries.push(serde_json::json!({"googleSearch": {}}));
                    }
                    "__builtin__code_execution" => {
                        builtin_entries.push(serde_json::json!({"codeExecution": {}}));
                    }
                    "__builtin__google_search_retrieval" => {
                        builtin_entries.push(serde_json::json!({"googleSearchRetrieval": {}}));
                    }
                    _ => {
                        let mut decl = serde_json::json!({"name": t.name});
                        let d = decl.as_object_mut().unwrap();
                        if let Some(ref desc) = t.description {
                            d.insert("description".into(), Value::String(desc.clone()));
                        }
                        d.insert("parameters".into(), sanitize_gemini_schema(&t.parameters));
                        fn_decls.push(decl);
                    }
                }
            }

            let mut tool_array: Vec<Value> = Vec::new();
            if !fn_decls.is_empty() {
                tool_array.push(serde_json::json!({"functionDeclarations": fn_decls}));
            }
            tool_array.extend(builtin_entries);

            if !tool_array.is_empty() {
                obj.insert("tools".into(), Value::Array(tool_array));
            }
        }

        // ── PR-11 extra passthrough fields ────────────────────────────────────
        if let Some(v) = req.extra.get("__google_tool_config") {
            obj.insert("toolConfig".into(), v.clone());
        }
        if let Some(v) = req.extra.get("__google_safety_settings") {
            obj.insert("safetySettings".into(), v.clone());
        }
        if let Some(v) = req.extra.get("__google_cached_content") {
            obj.insert("cachedContent".into(), v.clone());
        }

        Ok((body, HeaderMap::new()))
    }

    fn egress_path(&self, model: &str, stream: bool) -> String {
        if stream {
            format!("/v1beta/models/{}:streamGenerateContent?alt=sse", model)
        } else {
            format!("/v1beta/models/{}:generateContent", model)
        }
    }
}

// ── Schema sanitisation ───────────────────────────────────────────────────────

fn sanitize_gemini_schema(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            let mut notes = Vec::new();
            for (k, v) in map {
                if let Some(note) = gemini_schema_compatibility_note(k, v) {
                    notes.push(note);
                }
                if is_gemini_unsupported_schema_keyword(k) {
                    continue;
                }
                out.insert(k.clone(), sanitize_gemini_schema(v));
            }
            reconcile_required_with_properties(&mut out, &mut notes);
            append_description_notes(&mut out, notes);
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(sanitize_gemini_schema).collect()),
        _ => value.clone(),
    }
}

fn reconcile_required_with_properties(
    out: &mut serde_json::Map<String, Value>,
    notes: &mut Vec<String>,
) {
    let property_names = out
        .get("properties")
        .and_then(|value| value.as_object())
        .map(|properties| {
            properties
                .keys()
                .cloned()
                .collect::<std::collections::BTreeSet<_>>()
        });

    let Some(required) = out
        .get_mut("required")
        .and_then(|value| value.as_array_mut())
    else {
        return;
    };

    let Some(property_names) = property_names else {
        if !required.is_empty() {
            notes.push(
                "Some required fields were dropped because the matching properties are unavailable after Gemini compatibility sanitization.".to_string(),
            );
        }
        required.clear();
        return;
    };

    let mut dropped = Vec::new();
    required.retain(|entry| match entry.as_str() {
        Some(name) if property_names.contains(name) => true,
        Some(name) => {
            dropped.push(name.to_string());
            false
        }
        None => false,
    });

    if !dropped.is_empty() {
        notes.push(format!(
            "Required fields omitted for Gemini compatibility because their property definitions are unavailable: {}.",
            dropped.join(", ")
        ));
    }
}

fn append_description_notes(out: &mut serde_json::Map<String, Value>, notes: Vec<String>) {
    if notes.is_empty() {
        return;
    }

    let mut notes = notes;
    notes.dedup();
    let suffix = format!("Gemini compatibility hints: {}", notes.join(" "));

    match out.get_mut("description") {
        Some(Value::String(description)) => {
            if !description.contains(&suffix) {
                if !description.is_empty() {
                    description.push_str("\n\n");
                }
                description.push_str(&suffix);
            }
        }
        _ => {
            out.insert("description".into(), Value::String(suffix));
        }
    }
}

fn gemini_schema_compatibility_note(key: &str, value: &Value) -> Option<String> {
    match key {
        "additionalProperties" => match value {
            Value::Bool(false) => Some("Additional properties are not allowed.".to_string()),
            Value::Bool(true) => Some("Additional properties are allowed.".to_string()),
            _ => Some(
                "Additional properties have schema constraints that Gemini does not enforce directly."
                    .to_string(),
            ),
        },
        "propertyNames" => {
            if let Some(pattern) = value.get("pattern").and_then(|entry| entry.as_str()) {
                Some(format!(
                    "Object property names should match regex `{pattern}`."
                ))
            } else {
                Some(
                    "Object property names have extra validation rules that Gemini does not enforce directly."
                        .to_string(),
                )
            }
        }
        "exclusiveMinimum" => schema_number_note(
            value,
            "Value must be strictly greater than",
        ),
        "exclusiveMaximum" => schema_number_note(
            value,
            "Value must be strictly less than",
        ),
        "patternProperties" => Some(
            "Some property-name regex patterns carry additional value constraints."
                .to_string(),
        ),
        _ => None,
    }
}

fn schema_number_note(value: &Value, prefix: &str) -> Option<String> {
    match value {
        Value::Number(number) => Some(format!("{prefix} {number}.")),
        Value::String(text) => Some(format!("{prefix} {text}.")),
        _ => None,
    }
}

fn is_gemini_unsupported_schema_keyword(key: &str) -> bool {
    matches!(
        key,
        "$schema"
            | "additionalProperties"
            | "$ref"
            | "ref"
            | "definitions"
            | "$defs"
            | "propertyNames"
            | "patternProperties"
            | "unevaluatedProperties"
            | "dependentRequired"
            | "dependentSchemas"
            | "if"
            | "then"
            | "else"
            | "not"
            | "contains"
            | "minContains"
            | "maxContains"
            | "prefixItems"
            | "exclusiveMinimum"
            | "exclusiveMaximum"
    )
}

// ── Content encoding ──────────────────────────────────────────────────────────

fn encode_content(msg: &InternalMessage) -> Result<Value> {
    let role = match msg.role {
        Role::User | Role::Tool => "user",
        Role::Assistant => "model",
        Role::System => unreachable!("system handled separately"),
    };

    let parts = match &msg.content {
        MessageContent::Text(t) => {
            if msg.tool_call_id.is_some() {
                vec![serde_json::json!({
                    "functionResponse": {
                        "name": msg.tool_call_id,
                        "response": {"result": t}
                    }
                })]
            } else if let Some(ref tcs) = msg.tool_calls {
                let mut parts = Vec::new();
                if !t.is_empty() {
                    parts.push(serde_json::json!({"text": t}));
                }
                for tc in tcs {
                    let args: Value = serde_json::from_str(&tc.arguments)
                        .unwrap_or(Value::Object(Default::default()));
                    let mut function_call = serde_json::json!({"name": tc.name, "args": args});
                    let thought_signature = tc.thought_signature.clone().or_else(|| {
                        lookup_google_tool_call_thought_signature(
                            &msg.extra,
                            None,
                            Some(&tc.id),
                            Some(&tc.name),
                        )
                    });
                    if let Some(signature) = thought_signature
                        && let Some(obj) = function_call.as_object_mut()
                    {
                        obj.insert("thoughtSignature".into(), Value::String(signature));
                    }
                    parts.push(serde_json::json!({"functionCall": function_call}));
                }
                parts
            } else {
                vec![serde_json::json!({"text": t})]
            }
        }
        MessageContent::Blocks(blocks) => blocks
            .iter()
            .enumerate()
            .map(|(index, b)| match b {
                ContentBlock::Text { text } => serde_json::json!({"text": text}),
                ContentBlock::Image { source } => {
                    serde_json::json!({
                        "inlineData": {
                            "mimeType": source.media_type,
                            "data": source.data,
                        }
                    })
                }
                ContentBlock::ToolUse { id, name, input } => {
                    let mut function_call = serde_json::json!({"name": name, "args": input});
                    if let Some(signature) = lookup_google_tool_call_thought_signature(
                        &msg.extra,
                        Some(index),
                        Some(id),
                        Some(name),
                    ) && let Some(obj) = function_call.as_object_mut()
                    {
                        obj.insert("thoughtSignature".into(), Value::String(signature));
                    }
                    serde_json::json!({"functionCall": function_call})
                }
                ContentBlock::ToolResult {
                    tool_use_id,
                    content,
                } => {
                    serde_json::json!({
                        "functionResponse": {"name": tool_use_id, "response": content}
                    })
                }
                ContentBlock::Reasoning { text, .. } => {
                    serde_json::json!({"text": text})
                }
            })
            .collect(),
    };

    Ok(serde_json::json!({"role": role, "parts": parts}))
}
