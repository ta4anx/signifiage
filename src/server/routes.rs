//! Axum routes for the embedded web remote.
//!
//! Public static assets and the token-protected API live here; upload logic is
//! in [`super::uploads`]. The token travels in `X-Signifiage-Token` or, for
//! `EventSource` and the initial page load, `?token=`.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Multipart, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};

use super::actions::{RemoteAction, parse_action};
use super::{Mode, SharedState, uploads};

const INDEX_HTML: &str = include_str!("../../web/index.html");
const APP_JS: &str = include_str!("../../web/app.js");
const STYLE_CSS: &str = include_str!("../../web/style.css");
const I18N_JSON: &str = include_str!("../../web/i18n.json");

/// Everything a handler needs; one per running server.
pub(crate) struct AppState {
    pub(crate) shared: Arc<SharedState>,
    pub(crate) token: Arc<str>,
    pub(crate) mode: Mode,
    pub(crate) sender: mpsc::Sender<RemoteAction>,
    pub(crate) shutdown: watch::Receiver<bool>,
    pub(crate) upload_lock: tokio::sync::Mutex<()>,
}

/// Build the router with shared state attached.
pub(crate) fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/index.html", get(index))
        .route("/app.js", get(app_js))
        .route("/style.css", get(style_css))
        .route("/i18n.json", get(i18n_json))
        .route("/api/state", get(api_state))
        .route("/api/events", get(api_events))
        .route("/api/actions", post(api_actions))
        .route(
            "/api/upload",
            post(api_upload).layer(DefaultBodyLimit::disable()),
        )
        .with_state(state)
}

// --------------------------------------------------------------- static assets

fn text_response(body: &'static str, content_type: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    text_response(INDEX_HTML, "text/html; charset=utf-8")
}

async fn app_js() -> Response {
    text_response(APP_JS, "text/javascript; charset=utf-8")
}

async fn style_css() -> Response {
    text_response(STYLE_CSS, "text/css; charset=utf-8")
}

async fn i18n_json() -> Response {
    text_response(I18N_JSON, "application/json; charset=utf-8")
}

// -------------------------------------------------------------------- helpers

#[derive(Debug, Default, Deserialize)]
pub(crate) struct TokenQuery {
    #[serde(default)]
    pub(crate) token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActionRequest {
    action: String,
    #[serde(default)]
    args: Value,
}

/// Compare two tokens without early exit on the first differing byte.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.is_empty() || right.is_empty() {
        return false;
    }
    let mut diff = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

/// Header token wins; the query token is the EventSource fallback.
pub(crate) fn authorized(state: &AppState, headers: &HeaderMap, query_token: Option<&str>) -> bool {
    let provided = query_token.map(str::to_string).or_else(|| {
        headers
            .get("x-signifiage-token")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    });
    let ok =
        provided.is_some_and(|token| constant_time_eq(token.as_bytes(), state.token.as_bytes()));
    if !ok {
        // Never log the supplied or configured token.
        tracing::warn!("remote API request rejected: invalid or missing token");
    }
    ok
}

pub(crate) fn json_error(status: StatusCode, message: &str) -> Response {
    let body = serde_json::json!({ "ok": false, "error": message }).to_string();
    (
        status,
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        body,
    )
        .into_response()
}

pub(crate) fn unauthorized() -> Response {
    json_error(StatusCode::UNAUTHORIZED, "invalid or missing token")
}

pub(crate) fn no_project() -> Response {
    json_error(StatusCode::CONFLICT, "no project is open")
}

// ------------------------------------------------------------------- API state

async fn api_state(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    if !state.shared.has_project() {
        return no_project();
    }
    let latest = state.shared.latest();
    if latest.is_empty() {
        return no_project();
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        (*latest).clone(),
    )
        .into_response()
}

// -------------------------------------------------------------------- actions

async fn api_actions(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    body: Bytes,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    if !state.shared.has_project() {
        return no_project();
    }
    let request: ActionRequest = match serde_json::from_slice(&body) {
        Ok(request) => request,
        Err(error) => {
            return json_error(
                StatusCode::BAD_REQUEST,
                &format!("invalid action request: {error}"),
            );
        }
    };
    let action = match parse_action(&request.action, &request.args) {
        Ok(action) => action,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, &error.to_string()),
    };
    let name = action.name();
    let delivered = tokio::time::timeout(Duration::from_secs(2), state.sender.send(action)).await;
    match delivered {
        Ok(Ok(())) => Json(serde_json::json!({ "ok": true })).into_response(),
        _ => {
            tracing::warn!(action = name, "remote action was not delivered");
            json_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "the application is not accepting actions",
            )
        }
    }
}

// ---------------------------------------------------------------------- upload

async fn api_upload(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
    multipart: Multipart,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return unauthorized();
    }
    if state.mode == Mode::Player {
        return json_error(StatusCode::CONFLICT, "uploads require the desktop editor");
    }
    if !state.shared.has_project() {
        return no_project();
    }
    uploads::handle(state, multipart).await
}

// ------------------------------------------------------------------------ SSE

async fn api_events(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Query(query): Query<TokenQuery>,
) -> Response {
    if !authorized(&state, &headers, query.token.as_deref()) {
        return unauthorized();
    }

    let states = state.shared.subscribe();
    let shutdown = state.shutdown.clone();
    let stream = futures_util::stream::unfold(
        (states, shutdown, false),
        |(mut states, mut shutdown, initial_sent)| async move {
            let mut initial_sent = initial_sent;
            loop {
                let down = *shutdown.borrow();
                if down {
                    return None;
                }
                if !initial_sent {
                    initial_sent = true;
                    let data = { states.borrow().clone() };
                    if !data.is_empty() {
                        let event = Event::default().event("state").data((*data).clone());
                        return Some((
                            Ok::<_, Infallible>(event),
                            (states, shutdown, initial_sent),
                        ));
                    }
                }
                tokio::select! {
                    changed = states.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                        let data = { states.borrow().clone() };
                        if data.is_empty() {
                            continue;
                        }
                        let event = Event::default().event("state").data((*data).clone());
                        return Some((Ok::<_, Infallible>(event), (states, shutdown, initial_sent)));
                    }
                    changed = shutdown.changed() => {
                        if changed.is_err() {
                            return None;
                        }
                    }
                }
            }
        },
    );

    Sse::new(stream)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response()
}

/// Resolves when `stop()` flips the shutdown watch.
pub(crate) async fn wait_shutdown(mut shutdown: watch::Receiver<bool>) {
    loop {
        if *shutdown.borrow() {
            return;
        }
        if shutdown.changed().await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_comparison_requires_an_exact_non_empty_match() {
        assert!(constant_time_eq(b"token", b"token"));
        assert!(!constant_time_eq(b"token", b"toke"));
        assert!(!constant_time_eq(b"token", b"Token"));
        assert!(!constant_time_eq(b"", b""));
        assert!(!constant_time_eq(b"", b"token"));
        assert!(!constant_time_eq(b"token-long", b"token"));
    }

    #[test]
    fn embedded_assets_are_present_and_linked() {
        assert!(INDEX_HTML.contains("id=\"app\"") || INDEX_HTML.contains("class=\"app\""));
        assert!(INDEX_HTML.contains("app.js"));
        assert!(APP_JS.contains("/api/actions"));
        assert!(APP_JS.contains("EventSource"));
        assert!(STYLE_CSS.contains(".app"));
        let i18n: Value = serde_json::from_str(I18N_JSON).expect("i18n.json is valid");
        assert!(i18n.get("en").is_some());
        assert!(i18n.get("tr").is_some());
    }
}
