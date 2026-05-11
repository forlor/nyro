use std::borrow::Cow;
use std::sync::Arc;

use anyhow::{Context, bail};
use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::Value;

use crate::domain::DownstreamRouteKind;

#[derive(Debug, Clone)]
pub struct ProxyRouteRequest {
    pub group_id: String,
    pub route_kind: DownstreamRouteKind,
    pub model_action: Option<String>,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub json_body: Option<Arc<Value>>,
    pub diagnostics_enabled: bool,
}

impl ProxyRouteRequest {
    pub fn new(
        route_kind: DownstreamRouteKind,
        model_action: Option<String>,
        headers: HeaderMap,
        body: Bytes,
    ) -> anyhow::Result<Self> {
        let diagnostics_enabled = headers
            .get("x-nyro-race-diagnostics")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| {
                value.eq_ignore_ascii_case("1") || value.eq_ignore_ascii_case("true")
            });
        let (group_id, json_body) =
            resolve_group_id_and_body(route_kind, model_action.as_deref(), &body)?;

        Ok(Self {
            group_id,
            route_kind,
            model_action,
            headers,
            body,
            json_body,
            diagnostics_enabled,
        })
    }
}

fn resolve_group_id_and_body(
    route_kind: DownstreamRouteKind,
    model_action: Option<&str>,
    body: &[u8],
) -> anyhow::Result<(String, Option<Arc<Value>>)> {
    let (model, json_body) = match route_kind {
        DownstreamRouteKind::OpenAiChatCompletions
        | DownstreamRouteKind::OpenAiResponses
        | DownstreamRouteKind::AnthropicMessages => extract_model_and_json_body(body)?,
        DownstreamRouteKind::GoogleV1BetaModels | DownstreamRouteKind::GoogleV1Models => {
            (extract_model_from_google_action(model_action)?, None)
        }
    };

    let trimmed = model.trim();
    if trimmed.is_empty() {
        bail!("request model is required");
    }

    Ok((trimmed.to_string(), json_body))
}

fn extract_model_and_json_body(body: &[u8]) -> anyhow::Result<(String, Option<Arc<Value>>)> {
    let json_body =
        Arc::new(serde_json::from_slice::<Value>(body).context("invalid json request body")?);
    let payload = ModelFieldOnly::from_json(json_body.as_ref())?;
    let model = payload
        .model
        .map(Cow::into_owned)
        .context("request model is required")?;
    Ok((model, Some(json_body)))
}

fn extract_model_from_google_action(model_action: Option<&str>) -> anyhow::Result<String> {
    let raw = model_action.context("google model action is required")?;
    let (model, action) = raw
        .split_once(':')
        .context("google model action must use '<group_id>:<action>' format")?;
    let model = model.trim();
    let action = action.trim();
    if model.is_empty() || action.is_empty() {
        bail!("google model action is invalid");
    }
    Ok(model.to_string())
}

#[derive(Debug)]
struct ModelFieldOnly<'a> {
    model: Option<Cow<'a, str>>,
}

impl<'a> ModelFieldOnly<'a> {
    fn from_json(value: &'a Value) -> anyhow::Result<Self> {
        let model = value
            .get("model")
            .map(|model| {
                model
                    .as_str()
                    .map(Cow::Borrowed)
                    .context("request model must be a string")
            })
            .transpose()?;
        Ok(Self { model })
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderMap;

    use super::ProxyRouteRequest;
    use crate::domain::DownstreamRouteKind;

    #[test]
    fn openai_request_uses_body_model_as_group_id() {
        let request = ProxyRouteRequest::new(
            DownstreamRouteKind::OpenAiChatCompletions,
            None,
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":"group-a","stream":true}"#),
        )
        .expect("request");
        assert_eq!(request.group_id, "group-a");
        assert!(request.json_body.is_some());
    }

    #[test]
    fn anthropic_request_requires_model() {
        let error = ProxyRouteRequest::new(
            DownstreamRouteKind::AnthropicMessages,
            None,
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"max_tokens":128}"#),
        )
        .expect_err("missing model should fail");
        assert!(error.to_string().contains("request model is required"));
    }

    #[test]
    fn google_request_uses_model_action_prefix_as_group_id() {
        let request = ProxyRouteRequest::new(
            DownstreamRouteKind::GoogleV1BetaModels,
            Some("group-a:streamGenerateContent".to_string()),
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"contents":[]}"#),
        )
        .expect("request");
        assert_eq!(request.group_id, "group-a");
        assert!(request.json_body.is_none());
    }

    #[test]
    fn google_request_requires_action_suffix() {
        let error = ProxyRouteRequest::new(
            DownstreamRouteKind::GoogleV1BetaModels,
            Some("group-a".to_string()),
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"contents":[]}"#),
        )
        .expect_err("missing action should fail");
        assert!(
            error
                .to_string()
                .contains("google model action must use '<group_id>:<action>' format")
        );
    }

    #[test]
    fn openai_request_requires_string_model() {
        let error = ProxyRouteRequest::new(
            DownstreamRouteKind::OpenAiChatCompletions,
            None,
            HeaderMap::new(),
            bytes::Bytes::from_static(br#"{"model":{"id":"group-a"}}"#),
        )
        .expect_err("non-string model should fail");
        assert!(error.to_string().contains("request model must be a string"));
    }
}
