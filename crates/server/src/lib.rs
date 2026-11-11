//! The gateway: authentication, the session API and the embedded web UI.
//!
//! Everything under `/api` except `health` and `login` requires the access
//! token, as a `Bearer` header or the cookie set by `POST /api/login`.
//! Requests carrying an `Origin` that does not match `Host` are refused, so
//! another site open in the same browser cannot drive the daemon.

mod auth;
mod status;
mod ws;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{middleware, Json, Router};
use serde_json::json;
use unchained_core::SessionManager;
use unchained_protocol::CreateSession;

pub use auth::load_or_create_token;

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<SessionManager>,
    pub token: Arc<str>,
    /// Clients currently attached over a WebSocket.
    clients: Arc<AtomicUsize>,
    host: status::SharedSnapshot,
}

impl AppState {
    pub fn new(manager: SessionManager, token: impl Into<Arc<str>>) -> Self {
        Self {
            manager: Arc::new(manager),
            token: token.into(),
            clients: Arc::default(),
            host: Arc::default(),
        }
    }
}

#[derive(serde::Deserialize)]
struct PathParam {
    path: Option<String>,
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/status", get(get_status))
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/{name}/files", get(list_files))
        .route("/workspaces/{name}/file", get(read_file))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}/ws", get(ws::handler))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::require_token,
        ));
    let api = Router::new()
        .route("/health", get(health))
        .route("/login", post(auth::login))
        .route("/logout", post(auth::logout))
        .merge(protected)
        .layer(middleware::from_fn(auth::same_origin));
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/hud.js", get(hud_js))
        .route("/demo.js", get(demo_js))
        .route("/style.css", get(style_css))
        .nest("/api", api)
        .with_state(state)
}

/// Bind and serve until Ctrl-C. Refuses a non-loopback address unless
/// `allow_remote` is set: the daemon is private by default.
pub async fn serve(state: AppState, bind: SocketAddr, allow_remote: bool) -> Result<()> {
    if !bind.ip().is_loopback() && !allow_remote {
        bail!(
            "{bind} is not a loopback address. Unchained listens on localhost by default; \
             reach it over an SSH tunnel or a private network, or pass --allow-remote \
             if you understand what you are exposing."
        );
    }
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    tracing::info!("listening on http://{}", listener.local_addr()?);
    status::spawn_sampler(Arc::clone(&state.host));
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

async fn get_status(State(st): State<AppState>) -> impl IntoResponse {
    let host = st.host.read().unwrap().clone();
    let sessions = st.manager.list();
    Json(json!({
        "host": host,
        "agent": {
            "version": env!("CARGO_PKG_VERSION"),
            // No model adapter exists yet; the stub has no context or speed.
            "model": { "name": "stub", "ctx_used": null, "ctx_max": null, "tps": null },
            "mode": st.manager.mode().as_str(),
            "sessions": sessions.len(),
            "running": sessions.iter().filter(|s| s.running).count(),
            "pending": st.manager.pending_approvals(),
            "clients": st.clients.load(Ordering::Relaxed),
        },
    }))
}

fn bad_request(e: anyhow::Error) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": format!("{e:#}") })),
    )
        .into_response()
}

async fn list_files(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st
        .manager
        .list_files(&name, q.path.as_deref().unwrap_or("."))
        .await
    {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn read_file(
    State(st): State<AppState>,
    Path(name): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st
        .manager
        .read_file(&name, q.path.as_deref().unwrap_or(""))
        .await
    {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn list_workspaces(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.manager.workspaces())
}

async fn list_sessions(State(st): State<AppState>) -> impl IntoResponse {
    Json(st.manager.list())
}

async fn create_session(State(st): State<AppState>, Json(body): Json<CreateSession>) -> Response {
    match st.manager.create(&body.workspace) {
        Ok(session) => (StatusCode::CREATED, Json(session.info())).into_response(),
        Err(e) => bad_request(e),
    }
}

fn asset(content_type: &'static str, body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'self'; frame-ancestors 'none'; base-uri 'none'",
                ),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
        ],
        body,
    )
        .into_response()
}

async fn index() -> Response {
    asset(
        "text/html; charset=utf-8",
        include_str!("../../../web/index.html"),
    )
}

async fn app_js() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../../web/app.js"),
    )
}

async fn hud_js() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../../web/hud.js"),
    )
}

async fn demo_js() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../../web/demo.js"),
    )
}

async fn style_css() -> Response {
    asset(
        "text/css; charset=utf-8",
        include_str!("../../../web/style.css"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    use unchained_core::{Mode, Policy, StubModel, ToolRegistry, Workspace};

    fn app() -> (Router, tempfile::TempDir, tempfile::TempDir) {
        let data = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let manager = SessionManager::open(
            data.path(),
            vec![Workspace::new("demo", root.path()).unwrap()],
            Policy::new(Mode::Ask),
            Arc::new(StubModel),
            Arc::new(ToolRegistry::builtin()),
        )
        .unwrap();
        (router(AppState::new(manager, "sekrit")), data, root)
    }

    async fn status(app: &Router, req: Request<Body>) -> StatusCode {
        app.clone().oneshot(req).await.unwrap().status()
    }

    fn get(uri: &str) -> axum::http::request::Builder {
        Request::builder().uri(uri).header("host", "box:7878")
    }

    #[tokio::test]
    async fn api_requires_the_token() {
        let (app, _d, _r) = app();
        let empty = || Body::empty();
        assert_eq!(
            status(&app, get("/api/health").body(empty()).unwrap()).await,
            200
        );
        assert_eq!(status(&app, get("/").body(empty()).unwrap()).await, 200);
        for uri in [
            "/api/sessions",
            "/api/workspaces",
            "/api/sessions/x/ws",
            "/api/status",
            "/api/workspaces/demo/files",
            "/api/workspaces/demo/file?path=a",
        ] {
            assert_eq!(
                status(&app, get(uri).body(empty()).unwrap()).await,
                401,
                "{uri}"
            );
        }
        let wrong = get("/api/sessions").header("authorization", "Bearer nope");
        assert_eq!(status(&app, wrong.body(empty()).unwrap()).await, 401);
        let bearer = get("/api/sessions").header("authorization", "Bearer sekrit");
        assert_eq!(status(&app, bearer.body(empty()).unwrap()).await, 200);
        let cookie = get("/api/sessions").header("cookie", "a=b; unchained_token=sekrit");
        assert_eq!(status(&app, cookie.body(empty()).unwrap()).await, 200);
    }

    #[tokio::test]
    async fn login_sets_cookie_only_for_the_right_token() {
        let (app, _d, _r) = app();
        let login = |token: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("host", "box:7878")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "token": token }).to_string()))
                .unwrap()
        };
        let bad = app.clone().oneshot(login("nope")).await.unwrap();
        assert_eq!(bad.status(), 401);
        assert!(bad.headers().get("set-cookie").is_none());
        let good = app.clone().oneshot(login("sekrit")).await.unwrap();
        assert_eq!(good.status(), 204);
        let cookie = good.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    }

    #[tokio::test]
    async fn cross_origin_requests_are_refused() {
        let (app, _d, _r) = app();
        let req = |origin: &str| {
            get("/api/sessions")
                .header("authorization", "Bearer sekrit")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(status(&app, req("http://box:7878")).await, 200);
        assert_eq!(status(&app, req("https://evil.example")).await, 403);
        assert_eq!(status(&app, req("null")).await, 403);
    }

    #[tokio::test]
    async fn sessions_are_created_only_in_registered_workspaces() {
        let (app, _d, _r) = app();
        let create = |ws: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/sessions")
                .header("host", "box:7878")
                .header("authorization", "Bearer sekrit")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "workspace": ws }).to_string()))
                .unwrap()
        };
        assert_eq!(status(&app, create("demo")).await, 201);
        assert_eq!(status(&app, create("/etc")).await, 400);
    }

    #[tokio::test]
    async fn file_browser_stays_inside_the_workspace() {
        let (app, _d, root) = app();
        std::fs::write(root.path().join("a.txt"), "alpha").unwrap();
        let authed = |uri: &str| {
            get(uri)
                .header("authorization", "Bearer sekrit")
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(
            status(&app, authed("/api/workspaces/demo/files")).await,
            200
        );
        assert_eq!(
            status(&app, authed("/api/workspaces/demo/file?path=a.txt")).await,
            200
        );
        assert_eq!(status(&app, authed("/api/status")).await, 200);
        for uri in [
            "/api/workspaces/demo/files?path=..",
            "/api/workspaces/demo/file?path=../x",
            "/api/workspaces/demo/file?path=/etc/passwd",
            "/api/workspaces/nope/files",
        ] {
            assert_eq!(status(&app, authed(uri)).await, 400, "{uri}");
        }
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind_by_default() {
        let (_, data, root) = app();
        let manager = SessionManager::open(
            data.path(),
            vec![Workspace::new("demo", root.path()).unwrap()],
            Policy::new(Mode::Ask),
            Arc::new(StubModel),
            Arc::new(ToolRegistry::builtin()),
        )
        .unwrap();
        let err = serve(
            AppState::new(manager, "t"),
            "0.0.0.0:0".parse().unwrap(),
            false,
        )
        .await
        .unwrap_err();
        assert!(err.to_string().contains("--allow-remote"));
    }
}
