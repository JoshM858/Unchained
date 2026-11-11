use std::fs;
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use subtle::ConstantTimeEq;
use unchained_protocol::Login;

use crate::AppState;

const COOKIE: &str = "unchained_token";

/// Read the access token from `<data_dir>/token`, creating it (mode 0600,
/// 256 bits from the OS random source) on first run.
pub fn load_or_create_token(data_dir: &Path) -> Result<String> {
    let path = data_dir.join("token");
    if path.is_file() {
        let token = fs::read_to_string(&path)?.trim().to_string();
        if token.len() >= 32 {
            return Ok(token);
        }
        anyhow::bail!(
            "{} is too short to be a valid token; delete it to regenerate",
            path.display()
        );
    }
    fs::create_dir_all(data_dir)?;
    let token = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(&path)
        .and_then(|mut f| f.write_all(token.as_bytes()))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(token)
}

fn matches(candidate: &str, token: &str) -> bool {
    candidate.as_bytes().ct_eq(token.as_bytes()).into()
}

fn presented_token(headers: &HeaderMap) -> Option<&str> {
    if let Some(bearer) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
    {
        return Some(bearer);
    }
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|pair| pair.trim().split_once('='))
        .find(|(name, _)| *name == COOKIE)
        .map(|(_, value)| value)
}

pub async fn require_token(State(st): State<AppState>, req: Request, next: Next) -> Response {
    match presented_token(req.headers()) {
        Some(candidate) if matches(candidate, &st.token) => next.run(req).await,
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

/// Browsers attach `Origin` to cross-site requests and to every WebSocket
/// handshake. If it is present it must name the host we were addressed as.
pub async fn same_origin(req: Request, next: Next) -> Response {
    let headers = req.headers();
    if let Some(origin) = headers.get(header::ORIGIN) {
        let host = headers.get(header::HOST).and_then(|v| v.to_str().ok());
        let origin_host = origin
            .to_str()
            .ok()
            .and_then(|o| o.split_once("://"))
            .map(|(_, authority)| authority);
        if host.is_none() || origin_host != host {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    next.run(req).await
}

pub async fn login(State(st): State<AppState>, Json(body): Json<Login>) -> Response {
    if !matches(body.token.trim(), &st.token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    // No `Secure` attribute: the daemon speaks plain HTTP on localhost or a
    // private network. Put TLS in front before using a public hostname.
    let cookie = format!(
        "{COOKIE}={}; HttpOnly; SameSite=Strict; Path=/; Max-Age=2592000",
        st.token
    );
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}

pub async fn logout() -> Response {
    let cookie = format!("{COOKIE}=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0");
    (StatusCode::NO_CONTENT, [(header::SET_COOKIE, cookie)]).into_response()
}
