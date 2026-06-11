//! axum HTTP transport (SPEC §5.1, §6). Wraps the [`crate::service::Service`]
//! with the `/v1` routes and the common response envelope. Each handler maps a
//! `Result<T>` into either a success envelope or an error envelope with the
//! appropriate HTTP status.

use std::sync::Arc;

use axum::extract::Query;
use axum::extract::State;
use axum::http::header;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::response::Response;
use axum::routing::get;
use axum::routing::post;
use axum::Json;
use axum::Router;
use serde::Serialize;
use serde_json::Value;

use crate::error::Error;
use crate::ids;
use crate::protocol::Envelope;
use crate::protocol::ErrorBody;
use crate::protocol::ExportQuery;
use crate::protocol::ProviderTag;
use crate::service::Service;
use crate::PROVIDER_NAME;
use crate::PROVIDER_VERSION;

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub service: Service,
}

fn provider_tag() -> ProviderTag {
    ProviderTag {
        name: PROVIDER_NAME.to_string(),
        version: PROVIDER_VERSION.to_string(),
    }
}

/// Build the axum router with all `/v1` routes.
pub fn router(service: Service) -> Router {
    let state = AppState { service };
    Router::new()
        .route("/v1/status", get(status_handler))
        .route("/healthz", get(health_handler))
        .route("/v1/recall", post(recall_handler))
        .route("/v1/search", post(search_handler))
        .route("/v1/turns", post(turns_handler))
        .route("/v1/conclusions", post(conclusions_handler))
        .route("/v1/checkpoints", post(checkpoints_handler))
        .route("/v1/dream", post(dream_handler))
        .route("/v1/sync/local-codex-memory", post(sync_handler))
        .route("/v1/forget", post(forget_handler))
        .route("/v1/export", get(export_handler))
        .with_state(Arc::new(state))
}

/// Wrap a successful value in the success envelope (HTTP 200).
fn ok_envelope<T: Serialize>(data: T, warnings: Vec<String>) -> Response {
    let request_id = ids::new_id("req");
    let env = Envelope::success(data, warnings, request_id, provider_tag());
    (StatusCode::OK, Json(env)).into_response()
}

/// Wrap an error in the error envelope with its mapped HTTP status.
fn err_envelope(err: Error) -> Response {
    let status =
        StatusCode::from_u16(err.code.http_status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let request_id = ids::new_id("req");
    let env = Envelope::<Value> {
        ok: false,
        data: None,
        error: Some(ErrorBody::from(&err)),
        warnings: vec![],
        request_id,
        provider: provider_tag(),
    };
    (status, Json(env)).into_response()
}

/// Parse a JSON body manually so malformed JSON yields our envelope, not axum's
/// default plain-text 422.
async fn parse_body<T: serde::de::DeserializeOwned>(bytes: axum::body::Bytes) -> Result<T, Error> {
    if bytes.is_empty() {
        // Allow empty body to deserialize to default (all-optional structs).
        return serde_json::from_slice(b"{}").map_err(Error::from);
    }
    serde_json::from_slice(&bytes)
        .map_err(|e| Error::invalid_request(format!("invalid JSON body: {e}")))
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health_handler() -> Response {
    (StatusCode::OK, Json(serde_json::json!({ "ok": true }))).into_response()
}

async fn status_handler(State(state): State<Arc<AppState>>) -> Response {
    match state.service.status() {
        Ok(data) => {
            let warnings = data.degraded_reasons.clone();
            ok_envelope(data, warnings)
        }
        Err(e) => err_envelope(e),
    }
}

async fn recall_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.recall(req) {
        Ok(data) => ok_envelope(data, vec![]),
        Err(e) => err_envelope(e),
    }
}

async fn search_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.search(req) {
        Ok(data) => ok_envelope(data, vec![]),
        Err(e) => err_envelope(e),
    }
}

async fn turns_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.turns(req) {
        Ok(data) => {
            let warnings = if data.rejected > 0 {
                vec![format!("{} message(s) rejected by policy", data.rejected)]
            } else {
                vec![]
            };
            ok_envelope(data, warnings)
        }
        Err(e) => err_envelope(e),
    }
}

async fn conclusions_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.conclusions(req) {
        Ok(data) => {
            let warnings = if !data.rejected.is_empty() {
                vec![format!(
                    "{} conclusion(s) rejected by policy",
                    data.rejected.len()
                )]
            } else {
                vec![]
            };
            ok_envelope(data, warnings)
        }
        Err(e) => err_envelope(e),
    }
}

async fn checkpoints_handler(
    State(state): State<Arc<AppState>>,
    body: axum::body::Bytes,
) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.checkpoint(req) {
        Ok(data) => ok_envelope(data, vec![]),
        Err(e) => err_envelope(e),
    }
}

async fn dream_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.dream(req) {
        Ok(data) => ok_envelope(data, vec![]),
        Err(e) => err_envelope(e),
    }
}

async fn sync_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.sync_local(req) {
        Ok(data) => {
            let warnings = data.warnings.clone();
            ok_envelope(data, warnings)
        }
        Err(e) => err_envelope(e),
    }
}

async fn forget_handler(State(state): State<Arc<AppState>>, body: axum::body::Bytes) -> Response {
    let req = match parse_body(body).await {
        Ok(r) => r,
        Err(e) => return err_envelope(e),
    };
    match state.service.forget(req) {
        Ok(data) => ok_envelope(data, vec![]),
        Err(e) => err_envelope(e),
    }
}

async fn export_handler(
    State(state): State<Arc<AppState>>,
    Query(query): Query<ExportQuery>,
) -> Response {
    match state.service.export(query) {
        Ok(result) => {
            // Export streams the records directly (not wrapped in the envelope)
            // so it can be piped to a file; metadata goes in headers.
            (
                StatusCode::OK,
                [
                    (header::CONTENT_TYPE, result.content_type.to_string()),
                    (
                        "x-record-count".parse().unwrap(),
                        result.record_count.to_string(),
                    ),
                    (
                        "x-omitted-secret".parse().unwrap(),
                        result.omitted_secret.to_string(),
                    ),
                    (
                        "x-omitted-boundary".parse().unwrap(),
                        result.omitted_boundary.to_string(),
                    ),
                ],
                result.body,
            )
                .into_response()
        }
        Err(e) => err_envelope(e),
    }
}

/// Bind and serve until shutdown signal.
pub async fn serve(service: Service, bind: &str) -> anyhow::Result<()> {
    let app = router(service);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    let local = listener.local_addr()?;
    tracing::info!(bind = %local, "codex-memoryd HTTP server listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown signal received");
}
