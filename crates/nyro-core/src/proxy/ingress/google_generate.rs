//! Thin ingress shell: POST /v1beta/models/:model_action

use axum::extract::{Path, State};
use axum::http::{HeaderMap, Uri};
use axum::response::Response;
use axum::Json;
use serde_json::Value;

use crate::protocol::codec::google::decoder::GoogleDecoder;
use crate::protocol::ids::GOOGLE_GENERATE_V1BETA;
use crate::protocol::ir::{AiRequest, RawEnvelope};
use crate::proxy::context::RequestContext;
use crate::proxy::dispatcher::{dispatch_pipeline, error_response};
use crate::proxy::security::inject_query_api_key_header;
use crate::Gateway;

pub async fn google_generate(
    State(gw): State<Gateway>,
    mut ctx: axum::extract::Extension<RequestContext>,
    headers: HeaderMap,
    uri: Uri,
    Path(model_action): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    ctx.ingress_protocol = GOOGLE_GENERATE_V1BETA;
    let (model, action) = match model_action.rsplit_once(':') {
        Some((m, a)) => (m.to_string(), a.to_string()),
        None => (model_action.clone(), "generateContent".to_string()),
    };
    let is_stream = action == "streamGenerateContent";
    let path = format!("/v1beta/models/{model_action}");
    let flat_headers: std::collections::HashMap<String, String> = headers
        .iter()
        .filter_map(|(k, v)| v.to_str().ok().map(|vs| (k.as_str().to_lowercase(), vs.to_string())))
        .collect();
    let envelope = RawEnvelope::new(Some(body.clone()), flat_headers, "POST", &path);
    let internal = match GoogleDecoder.decode_with_model(body, &model, is_stream) {
        Ok(r) => r,
        Err(e) => return error_response(400, &format!("invalid Gemini request: {e}")),
    };
    let request: AiRequest = internal.into();
    let mut auth_headers = headers.clone();
    inject_query_api_key_header(&mut auth_headers, uri.query());
    dispatch_pipeline(gw, auth_headers, envelope, request, GOOGLE_GENERATE_V1BETA).await
}
