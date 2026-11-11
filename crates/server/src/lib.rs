//! The gateway: authentication, the project and session API, and the
//! embedded web UI.
//!
//! Everything under `/api` except `health` and `login` requires the access
//! token, as a `Bearer` header or the cookie set by `POST /api/login`.
//! Requests carrying an `Origin` that does not match `Host` are refused, so
//! another site open in the same browser cannot drive the daemon.
//!
//! HTML and SVG artifacts are served from `/frames/<key>/...`, outside the
//! API, under a content security policy that forbids the network. The key is
//! random per daemon start and is handed to signed-in clients only.

mod auth;
mod status;
mod ws;

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post};
use axum::{middleware, Json, Router};
use serde_json::json;
use subtle::ConstantTimeEq;
use unchained_core::artifacts::content_type;
use unchained_core::SessionManager;
use unchained_protocol::{CreateProject, CreateSession, UpdateProject};

pub use auth::load_or_create_token;

/// How often idle scratch space is checked for archiving.
const SWEEP_EVERY: Duration = Duration::from_secs(60 * 60);

#[derive(Clone)]
pub struct AppState {
    pub manager: Arc<SessionManager>,
    pub token: Arc<str>,
    /// Secret path segment for artifact frames; regenerated on every start.
    frame_key: Arc<str>,
    /// Clients currently attached over a WebSocket.
    clients: Arc<AtomicUsize>,
    host: status::SharedSnapshot,
}

impl AppState {
    pub fn new(manager: SessionManager, token: impl Into<Arc<str>>) -> Self {
        let key = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        Self {
            manager: Arc::new(manager),
            token: token.into(),
            frame_key: key.into(),
            clients: Arc::default(),
            host: Arc::default(),
        }
    }
}

#[derive(serde::Deserialize)]
struct PathParam {
    path: Option<String>,
}

#[derive(serde::Deserialize)]
struct VersionParam {
    version: Option<u32>,
}

#[derive(serde::Deserialize)]
struct ProjectFilter {
    project: Option<String>,
}

#[derive(serde::Deserialize)]
struct QueryParam {
    q: Option<String>,
}

pub fn router(state: AppState) -> Router {
    let protected = Router::new()
        .route("/status", get(get_status))
        .route("/workspaces", get(list_workspaces))
        .route("/workspaces/{name}/files", get(list_files))
        .route("/workspaces/{name}/file", get(read_file))
        .route("/projects", get(list_projects).post(create_project))
        .route("/projects/{id}", patch(update_project))
        .route("/projects/{id}/instructions", get(get_instructions))
        .route("/projects/{id}/notes", get(list_notes))
        .route("/projects/{id}/note", get(read_note))
        .route("/projects/{id}/backlinks", get(backlinks))
        .route("/projects/{id}/search", get(search_notes))
        .route("/projects/{id}/artifacts", get(list_artifacts))
        .route("/projects/{id}/artifacts/{name}", get(read_artifact))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/sessions/{id}", delete(delete_session))
        .route("/sessions/{id}/ws", get(ws::handler))
        .route("/sessions/{id}/scratch", get(list_scratch))
        .route("/sessions/{id}/scratch/file", get(read_scratch))
        .route("/archives", get(list_archives))
        .route("/archives/{name}/restore", post(restore_archive))
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
        .route("/md.js", get(md_js))
        .route("/favicon.ico", get(no_icon))
        .route("/demo.js", get(demo_js))
        .route("/style.css", get(style_css))
        .route("/frames/{key}/{project}/{artifact}/{version}", get(frame))
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
    spawn_scratch_sweeper(Arc::clone(&state.manager));
    axum::serve(listener, router(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

/// Archive idle scratch space now, then once an hour.
fn spawn_scratch_sweeper(manager: Arc<SessionManager>) {
    tokio::spawn(async move {
        loop {
            let m = Arc::clone(&manager);
            match tokio::task::spawn_blocking(move || m.sweep_scratch(SystemTime::now())).await {
                Ok(Ok(done)) => {
                    for a in done {
                        tracing::info!("archived scratch of session {} to {}", a.session, a.name);
                    }
                }
                Ok(Err(e)) => tracing::warn!("scratch sweep failed: {e:#}"),
                Err(e) => tracing::warn!("scratch sweep panicked: {e}"),
            }
            tokio::time::sleep(SWEEP_EVERY).await;
        }
    });
}

async fn health() -> Json<serde_json::Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

async fn get_status(State(st): State<AppState>) -> impl IntoResponse {
    let host = st.host.read().unwrap().clone();
    let sessions = st.manager.list(None);
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

fn not_found(message: &str) -> Response {
    (StatusCode::NOT_FOUND, Json(json!({ "error": message }))).into_response()
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

// ---- projects ----

async fn list_projects(State(st): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "projects": st.manager.projects(),
        // Relative, so the page works under any path prefix.
        "frame_base": format!("frames/{}", st.frame_key),
    }))
}

async fn create_project(State(st): State<AppState>, Json(body): Json<CreateProject>) -> Response {
    match st.manager.create_project(body) {
        Ok(project) => (StatusCode::CREATED, Json(project)).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn update_project(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<UpdateProject>,
) -> Response {
    match st.manager.update_project(&id, body) {
        Ok(project) => Json(project).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn get_instructions(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.manager.instructions(&id) {
        Ok(Some(i)) => Json(json!({ "file": i.file, "text": i.text })).into_response(),
        Ok(None) => Json(json!({ "file": null, "text": "" })).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn list_notes(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.manager.notes(&id) {
        Ok(notes) => Json(notes).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn read_note(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st.manager.note(&id, q.path.as_deref().unwrap_or("")) {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn backlinks(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st.manager.backlinks(&id, q.path.as_deref().unwrap_or("")) {
        Ok(paths) => Json(paths).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn search_notes(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<QueryParam>,
) -> Response {
    match st.manager.search_notes(&id, q.q.as_deref().unwrap_or("")) {
        Ok(hits) => Json(hits).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn list_artifacts(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    match st.manager.artifacts(&id) {
        Ok(items) => Json(items).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn read_artifact(
    State(st): State<AppState>,
    Path((id, name)): Path<(String, String)>,
    Query(q): Query<VersionParam>,
) -> Response {
    match st.manager.artifact(&id, &name, q.version) {
        Ok((meta, version, text)) => {
            Json(json!({ "meta": meta, "version": version, "text": text })).into_response()
        }
        Err(e) => bad_request(e),
    }
}

/// An artifact version as a document of its own. The response carries a
/// policy that allows scripts and styles inline but no network, forms or
/// navigation, and the frame is sandboxed, so the page cannot reach the
/// daemon even though it runs in the same browser.
async fn frame(
    State(st): State<AppState>,
    Path((key, project, artifact, version)): Path<(String, String, String, u32)>,
) -> Response {
    let ok: bool = key.as_bytes().ct_eq(st.frame_key.as_bytes()).into();
    if !ok {
        return not_found("no such frame");
    }
    let Ok((meta, _, text)) = st.manager.artifact(&project, &artifact, Some(version)) else {
        return not_found("no such artifact");
    };
    if meta.kind != "html" && meta.kind != "svg" {
        return not_found("only html and svg artifacts are framed");
    }
    (
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static(content_type(&meta.kind)),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(
                    "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; \
                     img-src data: blob:; font-src data:; connect-src 'none'; form-action 'none'; \
                     base-uri 'none'; frame-ancestors 'self'; sandbox allow-scripts",
                ),
            ),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
        text,
    )
        .into_response()
}

// ---- sessions ----

async fn list_sessions(
    State(st): State<AppState>,
    Query(q): Query<ProjectFilter>,
) -> impl IntoResponse {
    Json(st.manager.list(q.project.as_deref()))
}

async fn create_session(State(st): State<AppState>, Json(body): Json<CreateSession>) -> Response {
    match st.manager.create(&body.project) {
        Ok(session) => (StatusCode::CREATED, Json(session.info())).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn delete_session(State(st): State<AppState>, Path(id): Path<String>) -> Response {
    if st.manager.get(&id).is_none() {
        return not_found("no such session");
    }
    match st.manager.delete(&id) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => bad_request(e),
    }
}

async fn list_scratch(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st
        .manager
        .scratch_list(&id, q.path.as_deref().unwrap_or("."))
    {
        Ok(entries) => Json(entries).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn read_scratch(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Query(q): Query<PathParam>,
) -> Response {
    match st
        .manager
        .scratch_read(&id, q.path.as_deref().unwrap_or(""))
    {
        Ok(text) => Json(json!({ "text": text })).into_response(),
        Err(e) => bad_request(e),
    }
}

// ---- archives ----

async fn list_archives(State(st): State<AppState>) -> Response {
    match st.manager.archives() {
        Ok(list) => Json(list).into_response(),
        Err(e) => bad_request(e),
    }
}

async fn restore_archive(State(st): State<AppState>, Path(name): Path<String>) -> Response {
    match st.manager.restore_archive(&name) {
        Ok(session) => Json(json!({ "session": session })).into_response(),
        Err(e) => bad_request(e),
    }
}

// ---- static assets ----

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

/// The browser asks for an icon on every load. There is none, so answer 204
/// rather than a 404 that would show up in the console.
async fn no_icon() -> StatusCode {
    StatusCode::NO_CONTENT
}

async fn md_js() -> Response {
    asset(
        "text/javascript; charset=utf-8",
        include_str!("../../../web/md.js"),
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

    struct Fixture {
        app: Router,
        project: String,
        key: String,
        data: std::path::PathBuf,
        _dirs: (tempfile::TempDir, tempfile::TempDir),
        root: std::path::PathBuf,
    }

    fn fixture() -> Fixture {
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
        let project = manager
            .create_project(CreateProject {
                name: "p".into(),
                workspace: "demo".into(),
                vault: None,
                vault_mode: None,
            })
            .unwrap()
            .id;
        let state = AppState::new(manager, "sekrit");
        let key = state.frame_key.to_string();
        Fixture {
            app: router(state),
            project,
            key,
            data: data.path().to_path_buf(),
            root: root.path().to_path_buf(),
            _dirs: (data, root),
        }
    }

    async fn status(app: &Router, req: Request<Body>) -> StatusCode {
        app.clone().oneshot(req).await.unwrap().status()
    }

    async fn body_json(app: &Router, req: Request<Body>) -> serde_json::Value {
        let res = app.clone().oneshot(req).await.unwrap();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn get(uri: &str) -> axum::http::request::Builder {
        Request::builder().uri(uri).header("host", "box:7878")
    }

    fn authed(uri: &str) -> Request<Body> {
        get(uri)
            .header("authorization", "Bearer sekrit")
            .body(Body::empty())
            .unwrap()
    }

    fn post_json(uri: &str, method: &str, body: serde_json::Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "box:7878")
            .header("authorization", "Bearer sekrit")
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    #[tokio::test]
    async fn api_requires_the_token() {
        let f = fixture();
        let app = &f.app;
        let empty = || Body::empty();
        assert_eq!(
            status(app, get("/api/health").body(empty()).unwrap()).await,
            200
        );
        assert_eq!(status(app, get("/").body(empty()).unwrap()).await, 200);
        for uri in [
            "/api/sessions",
            "/api/workspaces",
            "/api/projects",
            "/api/sessions/x/ws",
            "/api/status",
            "/api/workspaces/demo/files",
            "/api/workspaces/demo/file?path=a",
            "/api/archives",
        ] {
            assert_eq!(
                status(app, get(uri).body(empty()).unwrap()).await,
                401,
                "{uri}"
            );
        }
        let wrong = get("/api/sessions").header("authorization", "Bearer nope");
        assert_eq!(status(app, wrong.body(empty()).unwrap()).await, 401);
        let bearer = get("/api/sessions").header("authorization", "Bearer sekrit");
        assert_eq!(status(app, bearer.body(empty()).unwrap()).await, 200);
        let cookie = get("/api/sessions").header("cookie", "a=b; unchained_token=sekrit");
        assert_eq!(status(app, cookie.body(empty()).unwrap()).await, 200);
    }

    #[tokio::test]
    async fn login_sets_cookie_only_for_the_right_token() {
        let f = fixture();
        let login = |token: &str| {
            Request::builder()
                .method("POST")
                .uri("/api/login")
                .header("host", "box:7878")
                .header("content-type", "application/json")
                .body(Body::from(json!({ "token": token }).to_string()))
                .unwrap()
        };
        let bad = f.app.clone().oneshot(login("nope")).await.unwrap();
        assert_eq!(bad.status(), 401);
        assert!(bad.headers().get("set-cookie").is_none());
        let good = f.app.clone().oneshot(login("sekrit")).await.unwrap();
        assert_eq!(good.status(), 204);
        let cookie = good.headers()["set-cookie"].to_str().unwrap();
        assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict"));
    }

    #[tokio::test]
    async fn cross_origin_requests_are_refused() {
        let f = fixture();
        let req = |origin: &str| {
            get("/api/sessions")
                .header("authorization", "Bearer sekrit")
                .header("origin", origin)
                .body(Body::empty())
                .unwrap()
        };
        assert_eq!(status(&f.app, req("http://box:7878")).await, 200);
        assert_eq!(status(&f.app, req("https://evil.example")).await, 403);
        assert_eq!(status(&f.app, req("null")).await, 403);
    }

    #[tokio::test]
    async fn sessions_belong_to_projects() {
        let f = fixture();
        let app = &f.app;
        assert_eq!(
            status(
                app,
                post_json("/api/sessions", "POST", json!({ "project": f.project }))
            )
            .await,
            201
        );
        assert_eq!(
            status(
                app,
                post_json("/api/sessions", "POST", json!({ "project": "nope" }))
            )
            .await,
            400
        );
        let listed = body_json(app, authed(&format!("/api/sessions?project={}", f.project))).await;
        assert_eq!(listed.as_array().unwrap().len(), 1);
        let none = body_json(app, authed("/api/sessions?project=other")).await;
        assert!(none.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn projects_validate_their_workspace_and_vault() {
        let f = fixture();
        let app = &f.app;
        assert_eq!(
            status(
                app,
                post_json(
                    "/api/projects",
                    "POST",
                    json!({ "name": "x", "workspace": "nope" })
                )
            )
            .await,
            400
        );
        assert_eq!(
            status(
                app,
                post_json(
                    "/api/projects",
                    "POST",
                    json!({ "name": "x", "workspace": "demo", "vault": "relative/path" })
                )
            )
            .await,
            400
        );
        let created = body_json(
            app,
            post_json(
                "/api/projects",
                "POST",
                json!({ "name": "Garden", "workspace": "demo" }),
            ),
        )
        .await;
        assert_eq!(created["vault_mode"], "read-only");
        // Notes are unavailable until a vault is attached.
        assert_eq!(
            status(app, authed(&format!("/api/projects/{}/notes", f.project))).await,
            400
        );
        let updated = body_json(
            app,
            post_json(
                &format!("/api/projects/{}", f.project),
                "PATCH",
                json!({ "vault_mode": "read-write-ask" }),
            ),
        )
        .await;
        assert_eq!(updated["vault_mode"], "read-write-ask");
    }

    #[tokio::test]
    async fn artifact_frames_need_the_key_and_are_sandboxed() {
        let f = fixture();
        // Save an HTML and a Markdown artifact where the manager keeps them.
        let store = unchained_core::ArtifactStore::new(
            f.data.join("projects").join(&f.project).join("artifacts"),
        );
        store.create("page", "html", "<h1>hello</h1>").unwrap();
        store.create("notes", "markdown", "# notes").unwrap();

        let listing = body_json(&f.app, authed("/api/projects")).await;
        let base = listing["frame_base"].as_str().unwrap().to_string();
        assert_eq!(base, format!("frames/{}", f.key));

        let frame_uri = format!("/{base}/{}/page/1", f.project);
        let res = f
            .app
            .clone()
            .oneshot(get(&frame_uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(res.status(), 200);
        let csp = res.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .to_string();
        assert!(csp.contains("sandbox allow-scripts"));
        assert!(csp.contains("connect-src 'none'"));
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert_eq!(&bytes[..], b"<h1>hello</h1>");

        // A wrong key, a missing artifact and a non-frameable kind are all refused.
        for uri in [
            format!("/frames/wrong/{}/page/1", f.project),
            format!("/frames/{}/{}/missing/1", f.key, f.project),
            format!("/frames/{}/{}/notes/1", f.key, f.project),
        ] {
            assert_eq!(
                status(&f.app, get(&uri).body(Body::empty()).unwrap()).await,
                404,
                "{uri}"
            );
        }
        // The frame path is not behind the API token: the key is the credential.
        let no_token = get(&frame_uri).body(Body::empty()).unwrap();
        assert_eq!(status(&f.app, no_token).await, 200);
    }

    #[tokio::test]
    async fn notes_and_scratch_are_reachable_through_the_api() {
        let f = fixture();
        let vault = tempfile::tempdir().unwrap();
        std::fs::write(vault.path().join("Home.md"), "See [[Plan]].").unwrap();
        std::fs::write(vault.path().join("Plan.md"), "the plan").unwrap();
        let attached = body_json(
            &f.app,
            post_json(
                &format!("/api/projects/{}", f.project),
                "PATCH",
                json!({ "vault": vault.path().display().to_string() }),
            ),
        )
        .await;
        assert!(attached["vault"].is_string());

        let notes = body_json(
            &f.app,
            authed(&format!("/api/projects/{}/notes", f.project)),
        )
        .await;
        assert_eq!(notes.as_array().unwrap().len(), 2);
        let back = body_json(
            &f.app,
            authed(&format!(
                "/api/projects/{}/backlinks?path=Plan.md",
                f.project
            )),
        )
        .await;
        assert_eq!(back, json!(["Home.md"]));
        let hits = body_json(
            &f.app,
            authed(&format!("/api/projects/{}/search?q=plan", f.project)),
        )
        .await;
        assert_eq!(hits.as_array().unwrap().len(), 2);
        assert_eq!(
            status(
                &f.app,
                authed(&format!("/api/projects/{}/note?path=../x.md", f.project))
            )
            .await,
            400
        );

        let session = body_json(
            &f.app,
            post_json("/api/sessions", "POST", json!({ "project": f.project })),
        )
        .await;
        let id = session["id"].as_str().unwrap().to_string();
        assert_eq!(
            status(&f.app, authed(&format!("/api/sessions/{id}/scratch"))).await,
            200
        );
        let deleted = f
            .app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/api/sessions/{id}"))
                    .header("host", "box:7878")
                    .header("authorization", "Bearer sekrit")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(deleted.status(), 204);
        assert_eq!(
            status(&f.app, authed(&format!("/api/sessions/{id}/scratch"))).await,
            400
        );
    }

    #[tokio::test]
    async fn file_browser_stays_inside_the_workspace() {
        let f = fixture();
        std::fs::write(f.root.join("a.txt"), "alpha").unwrap();
        let app = &f.app;
        assert_eq!(status(app, authed("/api/workspaces/demo/files")).await, 200);
        assert_eq!(
            status(app, authed("/api/workspaces/demo/file?path=a.txt")).await,
            200
        );
        assert_eq!(status(app, authed("/api/status")).await, 200);
        for uri in [
            "/api/workspaces/demo/files?path=..",
            "/api/workspaces/demo/file?path=../x",
            "/api/workspaces/demo/file?path=/etc/passwd",
            "/api/workspaces/nope/files",
        ] {
            assert_eq!(status(app, authed(uri)).await, 400, "{uri}");
        }
    }

    #[tokio::test]
    async fn refuses_non_loopback_bind_by_default() {
        let f = fixture();
        let data = tempfile::tempdir().unwrap();
        let manager = SessionManager::open(
            data.path(),
            vec![Workspace::new("demo", f.root.clone()).unwrap()],
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
