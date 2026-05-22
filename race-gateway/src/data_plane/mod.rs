mod request;
mod runner;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{HeaderMap, Response, StatusCode, header},
    response::IntoResponse,
    routing::{get, post},
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Serialize;

use crate::app::AppState;
use crate::domain::DownstreamRouteKind;

use self::request::ProxyRouteRequest;
use self::runner::RaceRunner;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/openai/v1/chat/completions", post(openai_chat_completions))
        .route("/openai/v1/responses", post(openai_responses))
        .route("/anthropic/v1/messages", post(anthropic_messages))
        .route(
            "/google/v1beta/models/:model_action",
            post(google_v1beta_models),
        )
        .route("/google/models/:model_action", post(google_v1_models))
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> axum::Json<HealthResponse> {
    let bind_addr = state.config.proxy_bind_addr.clone();
    axum::Json(HealthResponse {
        status: "ok",
        service: "race-gateway",
        bind_addr: bind_addr.clone(),
        proxy_bind_addr: bind_addr,
    })
}

async fn openai_chat_completions(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    proxy_route(
        state,
        ProxyRouteRequest::new(
            DownstreamRouteKind::OpenAiChatCompletions,
            None,
            headers,
            body,
        )
        .map_err(ProxyError::bad_request)?,
    )
    .await
}

async fn openai_responses(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    proxy_route(
        state,
        ProxyRouteRequest::new(DownstreamRouteKind::OpenAiResponses, None, headers, body)
            .map_err(ProxyError::bad_request)?,
    )
    .await
}

async fn anthropic_messages(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    proxy_route(
        state,
        ProxyRouteRequest::new(DownstreamRouteKind::AnthropicMessages, None, headers, body)
            .map_err(ProxyError::bad_request)?,
    )
    .await
}

async fn google_v1beta_models(
    State(state): State<AppState>,
    Path(model_action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    proxy_route(
        state,
        ProxyRouteRequest::new(
            DownstreamRouteKind::GoogleV1BetaModels,
            Some(model_action),
            headers,
            body,
        )
        .map_err(ProxyError::bad_request)?,
    )
    .await
}

async fn google_v1_models(
    State(state): State<AppState>,
    Path(model_action): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response<Body>, ProxyError> {
    proxy_route(
        state,
        ProxyRouteRequest::new(
            DownstreamRouteKind::GoogleV1Models,
            Some(model_action),
            headers,
            body,
        )
        .map_err(ProxyError::bad_request)?,
    )
    .await
}

async fn proxy_route(
    state: AppState,
    request: ProxyRouteRequest,
) -> Result<Response<Body>, ProxyError> {
    validate_proxy_auth(&state, &request.headers)?;

    let runner = RaceRunner::new(state.clone());
    let execution = runner
        .race_stream(request)
        .await
        .map_err(ProxyError::internal)?;

    let stream = execution.stream.map(Ok::<Bytes, std::convert::Infallible>);
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, execution.content_type);
    if let Some(diagnostics) = execution.diagnostics_header_value {
        response = response.header(crate::group::RACE_DIAGNOSTICS_HEADER, diagnostics);
    }
    response
        .body(Body::from_stream(stream))
        .map_err(|error| ProxyError::internal(anyhow::Error::new(error)))
}

fn validate_proxy_auth(state: &AppState, headers: &HeaderMap) -> Result<(), ProxyError> {
    let Some(configured_key) = state.config.proxy_api_key.as_deref() else {
        return Ok(());
    };

    // 1. Check Authorization header
    let auth_header_valid = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            let token = value.strip_prefix("Bearer ").unwrap_or(value).trim();
            token == configured_key
        })
        .unwrap_or(false);

    if auth_header_valid {
        return Ok(());
    }

    // 2. Check x-api-key header
    let api_key_valid = headers
        .get("x-api-key")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim() == configured_key)
        .unwrap_or(false);

    if api_key_valid {
        return Ok(());
    }

    Err(ProxyError::unauthorized("Unauthorized: missing or invalid API key".to_string()))
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
    bind_addr: String,
    proxy_bind_addr: String,
}

#[derive(Debug)]
struct ProxyError {
    status: StatusCode,
    message: String,
}

impl ProxyError {
    fn bad_request(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: error.to_string(),
        }
    }

    fn unauthorized(message: String) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message,
        }
    }

    fn internal(error: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ProxyError {
    fn into_response(self) -> Response<Body> {
        let body = serde_json::json!({
            "error": {
                "code": self.status.as_u16(),
                "message": self.message,
                "type": "RACE_GATEWAY_ERROR"
            }
        });
        Response::builder()
            .status(self.status)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .expect("build proxy error response")
    }
}
