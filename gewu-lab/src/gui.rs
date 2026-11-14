use crate::cli::GuiArgs;
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, Request, State},
    http::{HeaderValue, Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use gewu_lab::{Clock, LabError, LabStore, SystemClock};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const SESSION_TTL: Duration = Duration::from_secs(8 * 60 * 60);
const MAX_SESSIONS: usize = 1024;

#[derive(Clone)]
struct SessionCsrf(String);

struct AppState {
    store: LabStore,
    authority: String,
    token: Option<String>,
    csrf_token: String,
    secure_cookie: bool,
    allowed_origins: Vec<String>,
    sessions: Mutex<HashMap<String, Session>>,
    login_attempts: Mutex<Vec<Instant>>,
}

#[derive(Clone)]
struct Session {
    csrf: String,
    expires: Instant,
}

fn bind_address(host: &str, port: u16) -> anyhow::Result<SocketAddr> {
    let ip = match host {
        "localhost" => IpAddr::V4(Ipv4Addr::LOCALHOST),
        _ => host
            .parse::<IpAddr>()
            .map_err(|_| anyhow::anyhow!("host must be a literal IP or localhost"))?,
    };
    Ok(SocketAddr::new(ip, port))
}

pub async fn serve(store: LabStore, args: &GuiArgs, json_output: bool) -> anyhow::Result<()> {
    let address = bind_address(&args.host, args.port)?;
    let token = match &args.token {
        Some(token) => Some(token.clone()),
        None => std::env::var("GEWU_LAB_GUI_TOKEN")
            .map(Some)
            .or_else(|error| match error {
                std::env::VarError::NotPresent => Ok(None),
                _ => Err(anyhow::anyhow!("GEWU_LAB_GUI_TOKEN must be valid Unicode")),
            })?,
    };
    validate_token(address, token.as_deref())?;
    for origin in &args.allowed_origins {
        anyhow::ensure!(
            origin_authority(origin).is_some(),
            "allowed-origin must be an http(s) origin without path, credentials, query or fragment"
        );
    }
    if !address.ip().is_loopback() && !args.secure_cookie {
        eprintln!(
            "warning: LAN GUI uses plain HTTP; use --secure-cookie behind HTTPS in production"
        );
    }
    let listener = tokio::net::TcpListener::bind(address).await?;
    let address = listener.local_addr()?;
    let url = format!("http://{address}");
    if json_output {
        println!("{}", json!({"url": url, "worker_started": false}));
    } else {
        println!("GeWu Lab: {url} (dashboard only; no worker started)");
    }
    if args.open {
        let (program, arguments) = if cfg!(target_os = "macos") {
            ("open", vec![url.as_str()])
        } else if cfg!(target_os = "windows") {
            (
                "rundll32",
                vec!["url.dll,FileProtocolHandler", url.as_str()],
            )
        } else {
            ("xdg-open", vec![url.as_str()])
        };
        if let Err(error) = std::process::Command::new(program).args(arguments).spawn() {
            eprintln!("Could not open browser: {error}");
        }
    }
    let result = axum::serve(
        listener,
        router_with_options(
            store.clone(),
            address.to_string(),
            token,
            args.secure_cookie,
            args.allowed_origins.clone(),
        ),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await;
    store.pool().close().await;
    result?;
    Ok(())
}

fn validate_token(address: SocketAddr, token: Option<&str>) -> anyhow::Result<()> {
    anyhow::ensure!(
        token.is_none_or(|value| !value.is_empty()),
        "GUI token must not be empty"
    );
    anyhow::ensure!(
        token.is_none_or(|value| value.len() >= 32 && !value.trim().is_empty()),
        "GUI token must be at least 32 bytes"
    );
    anyhow::ensure!(
        address.ip().is_loopback() || token.is_some(),
        "non-loopback GUI binding requires --token or GEWU_LAB_GUI_TOKEN"
    );
    Ok(())
}

#[cfg(test)]
fn router(store: LabStore, authority: String) -> Router {
    router_with_options(store, authority, None, false, Vec::new())
}

fn router_with_options(
    store: LabStore,
    authority: String,
    token: Option<String>,
    secure_cookie: bool,
    allowed_origins: Vec<String>,
) -> Router {
    let state = Arc::new(AppState {
        store,
        authority,
        token,
        csrf_token: uuid::Uuid::new_v4().to_string(),
        secure_cookie,
        allowed_origins,
        sessions: Mutex::new(HashMap::new()),
        login_attempts: Mutex::new(Vec::new()),
    });
    router_with_state(state)
}

fn router_with_state(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(|| async { Html(include_str!("gui/index.html")) }))
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("gui/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("gui/app.js"),
                )
            }),
        )
        .route(
            "/fonts/inter.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    &include_bytes!("../../gewu-gui/assets/fonts/inter-var.woff2")[..],
                )
            }),
        )
        .route(
            "/fonts/mono.woff2",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "font/woff2")],
                    &include_bytes!("../../gewu-gui/assets/fonts/jetbrains-mono-400.woff2")[..],
                )
            }),
        )
        .route("/api/config", get(config))
        .route("/api/login", post(login))
        .route("/api/logout", post(logout))
        .route("/api/labs", get(labs))
        .route("/api/labs/{lab}", get(dashboard))
        .route("/api/labs/{lab}/{kind}/{id}/{action}", post(mutate))
        .fallback(|| async { ApiError(StatusCode::NOT_FOUND, "Not found".into()) })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), security))
        .with_state(state)
}

async fn security(
    State(state): State<Arc<AppState>>,
    mut request: Request,
    next: Next,
) -> Response {
    let headers = request.headers().clone();
    let value = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let path = request.uri().path();
    let public = !path.starts_with("/api/") || path == "/api/login";
    let session = value("cookie")
        .and_then(|cookie| cookie_value(cookie, "gewu_lab_session"))
        .and_then(|id| get_session(&state, &id));
    if !public && state.token.is_some() && session.is_none() {
        return security_headers(
            ApiError(StatusCode::UNAUTHORIZED, "Authentication required".into()).into_response(),
        );
    }
    let host = value("host").unwrap_or_default();
    let origin_allowed = |candidate: &str| {
        origin_authority(candidate).is_some_and(|authority| authority == host)
            && (candidate == format!("http://{host}") && direct_host(&state.authority, host)
                || state
                    .allowed_origins
                    .iter()
                    .any(|allowed| allowed == candidate))
    };
    let host_allowed = direct_host(&state.authority, host)
        || state
            .allowed_origins
            .iter()
            .any(|origin| origin_authority(origin).is_some_and(|authority| authority == host));
    let mutation = !matches!(*request.method(), Method::GET | Method::HEAD);
    let forbidden = !host_allowed
        || value("origin").is_some_and(|v| !origin_allowed(v))
        || value("sec-fetch-site").is_some_and(|v| !matches!(v, "same-origin" | "none"))
        || (mutation
            && (value("origin").is_none_or(|v| !origin_allowed(v))
                || (path != "/api/login"
                    && value("x-gewu-lab-csrf")
                        != session
                            .as_ref()
                            .map(|item| item.csrf.as_str())
                            .or(Some(state.csrf_token.as_str())))));
    let response = if forbidden {
        ApiError(
            StatusCode::FORBIDDEN,
            "Same-origin request and CSRF token required".into(),
        )
        .into_response()
    } else {
        request.extensions_mut().insert(SessionCsrf(
            session
                .as_ref()
                .map(|item| item.csrf.clone())
                .unwrap_or_else(|| state.csrf_token.clone()),
        ));
        next.run(request).await
    };
    security_headers(response)
}

fn security_headers(mut response: Response) -> Response {
    for (name, value) in [
        (
            "content-security-policy",
            "default-src 'none'; script-src 'self'; style-src 'self'; font-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'self'",
        ),
        ("x-content-type-options", "nosniff"),
        ("referrer-policy", "no-referrer"),
        ("cache-control", "no-store"),
    ] {
        response
            .headers_mut()
            .insert(name, HeaderValue::from_static(value));
    }
    response
}

fn cookie_value(cookie: &str, name: &str) -> Option<String> {
    cookie.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        (key == name).then(|| value.to_string())
    })
}

fn get_session(state: &AppState, id: &str) -> Option<Session> {
    let mut sessions = state.sessions.lock().ok()?;
    let item = sessions.get(id)?.clone();
    if item.expires <= Instant::now() {
        sessions.remove(id);
        None
    } else {
        Some(item)
    }
}

fn origin_authority(origin: &str) -> Option<String> {
    let uri = origin.parse::<axum::http::Uri>().ok()?;
    if !matches!(uri.scheme_str(), Some("http" | "https")) || origin.contains(['?', '#', '@']) {
        return None;
    }
    let authority = uri.authority()?.as_str();
    (origin == format!("{}://{authority}", uri.scheme_str()?)).then(|| authority.to_string())
}

fn direct_host(bound: &str, host: &str) -> bool {
    let Ok(bound) = bound.parse::<SocketAddr>() else {
        return false;
    };
    let Ok(authority) = host.parse::<axum::http::uri::Authority>() else {
        return false;
    };
    if host.contains('@')
        || !(host == authority.host()
            || host
                == format!(
                    "{}:{}",
                    authority.host(),
                    authority.port_u16().unwrap_or(80)
                ))
    {
        return false;
    }
    let port = authority.port_u16().unwrap_or(80);
    if port != bound.port() {
        return false;
    }
    let hostname = authority.host().trim_matches(['[', ']']);
    if hostname == "localhost" {
        return bound.ip().is_loopback() || bound.ip().is_unspecified();
    }
    hostname.parse::<IpAddr>().is_ok_and(|ip| {
        if bound.ip().is_unspecified() {
            !ip.is_unspecified()
        } else {
            ip == bound.ip()
        }
    })
}

struct ApiError(StatusCode, String);
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}
impl From<LabError> for ApiError {
    fn from(error: LabError) -> Self {
        match error {
            LabError::Invalid(message) => Self(StatusCode::CONFLICT, message),
            LabError::StaleAttempt => Self(
                StatusCode::CONFLICT,
                "Attempt is no longer reviewable".into(),
            ),
            _ => Self(
                StatusCode::INTERNAL_SERVER_ERROR,
                "Database operation failed".into(),
            ),
        }
    }
}
type ApiResult = Result<Json<Value>, ApiError>;
fn missing() -> ApiError {
    ApiError(
        StatusCode::NOT_FOUND,
        "Resource not found in this lab".into(),
    )
}

async fn config(
    State(state): State<Arc<AppState>>,
    Extension(csrf): Extension<SessionCsrf>,
) -> Json<Value> {
    Json(
        json!({"csrf_token": csrf.0, "auth_required": state.token.is_some(), "worker_started": false}),
    )
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Login {
    token: String,
}

fn tokens_equal(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    bool::from(a.as_bytes().ct_eq(b.as_bytes()))
}

async fn login(
    State(state): State<Arc<AppState>>,
    body: Result<Json<Login>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let now = Instant::now();
    {
        let mut attempts = state
            .login_attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        attempts.retain(|time| now.duration_since(*time) < Duration::from_secs(60));
        if attempts.len() >= 20 {
            return ApiError(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many login attempts".into(),
            )
            .into_response();
        }
        attempts.push(now);
    }
    let Ok(Json(body)) = body else {
        return ApiError(StatusCode::BAD_REQUEST, "Invalid login JSON".into()).into_response();
    };
    if !state
        .token
        .as_deref()
        .is_some_and(|expected| tokens_equal(&body.token, expected))
    {
        return ApiError(StatusCode::UNAUTHORIZED, "Invalid credentials".into()).into_response();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let csrf = uuid::Uuid::new_v4().to_string();
    {
        let mut sessions = state.sessions.lock().unwrap_or_else(|e| e.into_inner());
        sessions.retain(|_, item| item.expires > now);
        if sessions.len() >= MAX_SESSIONS {
            return ApiError(
                StatusCode::SERVICE_UNAVAILABLE,
                "Session capacity reached".into(),
            )
            .into_response();
        }
        sessions.insert(
            id.clone(),
            Session {
                csrf,
                expires: now + SESSION_TTL,
            },
        );
    }
    let secure = if state.secure_cookie { "; Secure" } else { "" };
    (
        [(
            header::SET_COOKIE,
            format!(
                "gewu_lab_session={id}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}{secure}",
                SESSION_TTL.as_secs()
            ),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}

async fn logout(State(state): State<Arc<AppState>>, request: Request) -> Response {
    if let Some(id) = request
        .headers()
        .get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| cookie_value(v, "gewu_lab_session"))
    {
        state
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }
    let secure = if state.secure_cookie { "; Secure" } else { "" };
    (
        [(
            header::SET_COOKIE,
            format!("gewu_lab_session=; HttpOnly; SameSite=Strict; Path=/; Max-Age=0{secure}"),
        )],
        Json(json!({"ok": true})),
    )
        .into_response()
}
async fn labs(State(state): State<Arc<AppState>>) -> ApiResult {
    Ok(Json(json!({"labs": state.store.labs().await?})))
}
async fn dashboard(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> ApiResult {
    let store = &state.store;
    let lab = store.get_lab(&id).await?.ok_or_else(missing)?;
    let projects = store.projects(&id).await?;
    let mut tasks = Vec::new();
    let mut artifacts = Vec::new();
    for project in &projects {
        for task in store.tasks(&project.id).await? {
            artifacts.extend(store.artifacts_for_task(&task.id).await?);
            tasks.push(task);
        }
    }
    let mut counts = std::collections::BTreeMap::new();
    for task in &tasks {
        let status = serde_json::to_value(&task.status).unwrap_or_default();
        *counts
            .entry(status.as_str().unwrap_or("unknown").to_owned())
            .or_insert(0usize) += 1;
    }
    let mut proposal_decisions = Vec::new();
    let proposals = store.proposals(&id).await?;
    for proposal in &proposals {
        proposal_decisions.extend(store.proposal_decisions(&proposal.id).await?);
    }
    Ok(Json(json!({
        "interest": store.interest_revision(&id, lab.current_interest_revision).await?,
        "lab": lab, "projects": projects, "tasks": tasks, "task_counts": counts,
        "proposals": proposals, "proposal_decisions": proposal_decisions,
        "attempts": store.attempts(&id).await?, "artifacts": artifacts,
        "decisions": store.decisions(&id).await?, "worker_started": false
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Mutation {
    actor: String,
    rationale: String,
    priority: Option<i64>,
}

async fn mutate(
    State(state): State<Arc<AppState>>,
    Path((lab, kind, id, action)): Path<(String, String, String, String)>,
    body: Result<Json<Mutation>, axum::extract::rejection::JsonRejection>,
) -> ApiResult {
    let Json(body) = body.map_err(|error| {
        ApiError(
            error.status(),
            "Invalid mutation JSON (maximum 16 KiB)".into(),
        )
    })?;
    if body.actor.trim().is_empty()
        || body.rationale.trim().is_empty()
        || body.actor.len() > 128
        || body.rationale.len() > 8192
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "Actor and rationale are required (limits 128 / 8192 bytes)".into(),
        ));
    }
    let store = &state.store;
    store.get_lab(&lab).await?.ok_or_else(missing)?;
    let associated = match kind.as_str() {
        "lab" => id == lab,
        "project" => store
            .get_project(&id)
            .await?
            .is_some_and(|p| p.lab_id == lab),
        "proposal" => store
            .get_proposal(&id)
            .await?
            .is_some_and(|p| p.lab_id == lab),
        "task" => {
            if let Some(task) = store.get_task(&id).await? {
                store
                    .get_project(&task.project_id)
                    .await?
                    .is_some_and(|p| p.lab_id == lab)
            } else {
                false
            }
        }
        "attempt" => store.attempts(&lab).await?.iter().any(|a| a.id == id),
        _ => false,
    };
    if !associated {
        return Err(missing());
    }
    let now = SystemClock.now();
    let (actor, rationale) = (body.actor.trim(), body.rationale.trim());
    match (kind.as_str(), action.as_str()) {
        ("lab", "pause" | "resume") => {
            store
                .set_paused(&id, action == "pause", actor, rationale, now)
                .await?
        }
        ("project", "approve") => store.approve_project(&id, actor, rationale, now).await?,
        ("proposal", "approve") => store.approve_proposal(&id, actor, rationale, now).await?,
        ("proposal", "reject") => store.reject_proposal(&id, actor, rationale, now).await?,
        ("task", "cancel") => store.cancel_task(&id, actor, rationale, now).await?,
        ("task", "priority") => {
            store
                .override_priority(
                    &id,
                    body.priority.ok_or_else(|| {
                        ApiError(StatusCode::BAD_REQUEST, "Integer priority required".into())
                    })?,
                    actor,
                    rationale,
                    now,
                )
                .await?
        }
        ("attempt", "accept" | "reject") => {
            store
                .review_result(&id, action == "accept", actor, rationale, now)
                .await?
        }
        _ => return Err(ApiError(StatusCode::BAD_REQUEST, "Unknown action".into())),
    }
    Ok(Json(json!({"ok": true})))
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    const SECRET: &str = "a-test-token-with-at-least-32-bytes";

    async fn request(
        app: &Router,
        path: &str,
        cookie: &str,
        csrf: &str,
        body: Option<Value>,
    ) -> Response {
        let mut builder = HttpRequest::builder()
            .uri(path)
            .header("host", "127.0.0.1:8440")
            .header("cookie", cookie);
        let body = if let Some(body) = body {
            builder = builder
                .method("POST")
                .header("origin", "http://127.0.0.1:8440")
                .header("x-gewu-lab-csrf", csrf)
                .header("content-type", "application/json");
            Body::from(body.to_string())
        } else {
            Body::empty()
        };
        app.clone()
            .oneshot(builder.body(body).unwrap())
            .await
            .unwrap()
    }

    async fn response_json(response: Response) -> Value {
        serde_json::from_slice(
            &to_bytes(response.into_body(), 4 * 1024 * 1024)
                .await
                .unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn binding_token_and_authority_validation() {
        for host in ["0.0.0.0", "::", "192.168.1.2"] {
            let address = bind_address(host, 8440).unwrap();
            for token in [None, Some(""), Some("short")] {
                assert!(validate_token(address, token).is_err());
            }
            assert!(validate_token(address, Some(SECRET)).is_ok());
        }
        let address = bind_address("localhost", 0).unwrap();
        assert!(validate_token(address, None).is_ok());
        assert!(validate_token(address, Some("")).is_err());
        assert!(validate_token(address, Some(SECRET)).is_ok());
        for host in ["192.168.1.2:8440", "localhost:8440", "[::1]:8440"] {
            assert!(direct_host("0.0.0.0:8440", host));
        }
        for host in [
            "evil.test:8440",
            "192.168.1.2:8441",
            "127.0.0.1.evil:8440",
            "user@127.0.0.1:8440",
        ] {
            assert!(!direct_host("0.0.0.0:8440", host));
        }
        for origin in [
            "null",
            "https://example.com/",
            "https://u@example.com",
            "https://example.com?x=y",
        ] {
            assert!(origin_authority(origin).is_none());
        }
        assert_eq!(
            origin_authority("https://example.com"),
            Some("example.com".into())
        );
        assert!(tokens_equal(SECRET, SECRET));
        assert!(!tokens_equal(SECRET, "wrong"));
    }

    #[tokio::test]
    async fn authenticated_sessions_csrf_expiry_logout_and_limits() {
        let (_dir, store, _) = fixture().await;
        let state = Arc::new(AppState {
            store,
            authority: "127.0.0.1:8440".into(),
            token: Some(SECRET.into()),
            csrf_token: "anonymous-csrf".into(),
            secure_cookie: true,
            allowed_origins: vec!["https://lab.example".into()],
            sessions: Mutex::new(HashMap::new()),
            login_attempts: Mutex::new(Vec::new()),
        });
        let app = router_with_state(state.clone());
        for path in ["/api/config", "/api/labs", "/api/labs/l"] {
            let response = request(&app, path, "", "", None).await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            assert!(!response.headers().contains_key(header::LOCATION));
        }
        assert_eq!(
            request(
                &app,
                "/api/login",
                "",
                "",
                Some(json!({"token": "invalid"}))
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED
        );
        let mut cookies = Vec::new();
        let mut csrf_tokens = Vec::new();
        for _ in 0..2 {
            let response =
                request(&app, "/api/login", "", "", Some(json!({"token": SECRET}))).await;
            assert_eq!(response.status(), StatusCode::OK);
            let cookie = response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .to_string();
            for attribute in [
                "HttpOnly",
                "SameSite=Strict",
                "Path=/",
                "Secure",
                "Max-Age=28800",
            ] {
                assert!(cookie.contains(attribute));
            }
            assert!(!cookie.contains(SECRET));
            let cookie = cookie.split(';').next().unwrap().to_string();
            let config = response_json(request(&app, "/api/config", &cookie, "", None).await).await;
            assert!(!config.to_string().contains(SECRET));
            csrf_tokens.push(config["csrf_token"].as_str().unwrap().to_string());
            cookies.push(cookie);
        }
        assert_ne!(cookies[0], cookies[1]);
        assert_ne!(csrf_tokens[0], csrf_tokens[1]);
        assert_eq!(
            request(&app, "/api/labs", &cookies[0], "", None)
                .await
                .status(),
            StatusCode::OK
        );
        let mutation = "/api/labs/l/lab/l/pause";
        let body = json!({"actor":"human", "rationale":"reviewed"});
        for token in [&csrf_tokens[1], &state.csrf_token, &String::new()] {
            assert_eq!(
                request(&app, mutation, &cookies[0], token, Some(body.clone()))
                    .await
                    .status(),
                StatusCode::FORBIDDEN
            );
        }
        for (host, origin) in [
            ("127.0.0.1:8440", "https://evil.example"),
            ("127.0.0.1:8440", "https://lab.example"),
            ("evil.example", "https://evil.example"),
        ] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri(mutation)
                        .header("host", host)
                        .header("origin", origin)
                        .header("cookie", &cookies[0])
                        .header("x-gewu-lab-csrf", &csrf_tokens[0])
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri("/api/config")
                    .header("host", "lab.example")
                    .header("origin", "https://lab.example")
                    .header("cookie", &cookies[0])
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            request(&app, mutation, &cookies[0], &csrf_tokens[0], Some(body))
                .await
                .status(),
            StatusCode::OK
        );
        let response = request(
            &app,
            "/api/logout",
            &cookies[0],
            &csrf_tokens[0],
            Some(json!({})),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            response.headers()[header::SET_COOKIE]
                .to_str()
                .unwrap()
                .contains("Max-Age=0")
        );
        assert_eq!(
            request(&app, "/api/config", &cookies[0], "", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert_eq!(
            request(&app, "/api/config", &cookies[1], "", None)
                .await
                .status(),
            StatusCode::OK
        );
        let id = cookie_value(&cookies[1], "gewu_lab_session").unwrap();
        state.sessions.lock().unwrap().get_mut(&id).unwrap().expires =
            Instant::now() - Duration::from_secs(1);
        assert_eq!(
            request(&app, "/api/config", &cookies[1], "", None)
                .await
                .status(),
            StatusCode::UNAUTHORIZED
        );
        assert!(state.sessions.lock().unwrap().is_empty());
        for index in 0..MAX_SESSIONS {
            state.sessions.lock().unwrap().insert(
                index.to_string(),
                Session {
                    csrf: "csrf".into(),
                    expires: Instant::now() + SESSION_TTL,
                },
            );
        }
        assert_eq!(
            request(&app, "/api/login", "", "", Some(json!({"token": SECRET})))
                .await
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
        state.sessions.lock().unwrap().clear();
        for _ in 0..20 {
            let _ = request(&app, "/api/login", "", "", Some(json!({"token":"wrong"}))).await;
        }
        assert_eq!(
            request(&app, "/api/login", "", "", Some(json!({"token": SECRET})))
                .await
                .status(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(state.login_attempts.lock().unwrap().len() <= 20);
    }

    async fn fixture() -> (tempfile::TempDir, LabStore, Router) {
        let dir = tempfile::tempdir().unwrap();
        let store = LabStore::connect(&format!("sqlite://{}", dir.path().join("lab.db").display()))
            .await
            .unwrap();
        store
            .create_lab("l", "<script>alert('lab')</script>", "Research", 1)
            .await
            .unwrap();
        store
            .create_lab("other", "Other", "Other research", 1)
            .await
            .unwrap();
        store
            .create_project("p", "l", "Project", "Why?", 1)
            .await
            .unwrap();
        store
            .create_project("foreign", "other", "Foreign", "Why?", 1)
            .await
            .unwrap();
        store
            .create_task(
                &gewu_lab::TaskSpec {
                    id: "t".into(),
                    project_id: "p".into(),
                    objective: "Investigate".into(),
                    context: "Context".into(),
                    deliverable: "Report".into(),
                    completion_criteria: "Evidence".into(),
                    owner: None,
                    priority: 1,
                },
                1,
            )
            .await
            .unwrap();
        let app = router(store.clone(), "127.0.0.1:8440".into());
        (dir, store, app)
    }

    async fn read(app: &Router, path: &str) -> (StatusCode, Value) {
        let response = app
            .clone()
            .oneshot(
                HttpRequest::builder()
                    .uri(path)
                    .header("host", "127.0.0.1:8440")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4 * 1024 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn token(app: &Router) -> String {
        read(app, "/api/config").await.1["csrf_token"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    async fn write(app: &Router, path: &str, token: &str, body: Value) -> StatusCode {
        app.clone()
            .oneshot(
                HttpRequest::builder()
                    .method("POST")
                    .uri(path)
                    .header("host", "127.0.0.1:8440")
                    .header("origin", "http://127.0.0.1:8440")
                    .header("x-gewu-lab-csrf", token)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }

    #[test]
    fn loopback_only_and_ephemeral_port() {
        for host in ["127.0.0.1", "::1", "localhost"] {
            assert_eq!(bind_address(host, 0).unwrap().port(), 0);
        }
        for host in ["0.0.0.0", "::", "192.168.1.2"] {
            assert!(bind_address(host, 8440).is_ok());
        }
        for host in ["example.com", "127.0.0.1.example.com"] {
            assert!(bind_address(host, 8440).is_err());
        }
    }

    #[tokio::test]
    async fn read_dashboard_is_isolated_and_does_not_launch_worker() {
        let (_dir, store, app) = fixture().await;
        assert_eq!(
            read(&app, "/api/labs").await.1["labs"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        let (status, value) = read(&app, "/api/labs/l").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(value["lab"]["name"], "<script>alert('lab')</script>");
        assert_eq!(value["projects"].as_array().unwrap().len(), 1);
        assert_eq!(value["task_counts"]["ready"], 1);
        assert_eq!(value["worker_started"], false);
        assert!(store.attempts("l").await.unwrap().is_empty());
        assert!(!value.to_string().contains("lease_token"));
        assert!(!value.to_string().contains("credentials"));
        assert_eq!(
            read(&app, "/api/labs/missing").await.0,
            StatusCode::NOT_FOUND
        );
        for path in [
            "/",
            "/app.css",
            "/app.js",
            "/fonts/inter.woff2",
            "/fonts/mono.woff2",
        ] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .uri(path)
                        .header("host", "127.0.0.1:8440")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(
                response.headers()["content-security-policy"]
                    .to_str()
                    .unwrap()
                    .contains("frame-ancestors 'none'")
            );
        }
        let js = include_str!("gui/app.js");
        assert!(!js.contains("innerHTML"));
        assert!(js.contains("textContent"));
    }

    #[tokio::test]
    async fn csrf_host_origin_and_invalid_mutations_are_rejected_without_audit() {
        let (_dir, store, app) = fixture().await;
        let token = token(&app).await;
        let body = json!({"actor": "human", "rationale": "reviewed"});
        assert_eq!(
            write(&app, "/api/labs/l/project/p/approve", "bad", body.clone()).await,
            StatusCode::FORBIDDEN
        );
        for (host, origin) in [
            ("evil.test:8440", "http://127.0.0.1:8440"),
            ("127.0.0.1:8440", "http://evil.test"),
            ("127.0.0.1:8440", "null"),
            ("127.0.0.1:8440", ""),
        ] {
            let response = app
                .clone()
                .oneshot(
                    HttpRequest::builder()
                        .method("POST")
                        .uri("/api/labs/l/project/p/approve")
                        .header("host", host)
                        .header("origin", origin)
                        .header("x-gewu-lab-csrf", &token)
                        .header("content-type", "application/json")
                        .body(Body::from(body.to_string()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::FORBIDDEN);
        }
        assert_eq!(
            write(
                &app,
                "/api/labs/l/project/foreign/approve",
                &token,
                body.clone()
            )
            .await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            write(&app, "/api/labs/l/project/p/destroy", &token, body.clone()).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            write(&app, "/api/labs/l/task/t/priority", &token, body).await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            write(
                &app,
                "/api/labs/l/project/p/approve",
                &token,
                json!({"actor":" ","rationale":"x"})
            )
            .await,
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            write(
                &app,
                "/api/labs/l/project/p/approve",
                &token,
                json!({"actor":"human","rationale":"x".repeat(20_000)})
            )
            .await,
            StatusCode::PAYLOAD_TOO_LARGE
        );
        assert!(store.decisions("l").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn proposals_and_cancellation_use_durable_store_decisions() {
        let (_dir, store, app) = fixture().await;
        let token = token(&app).await;
        let spec = gewu_lab::PlanningProposalSpec {
            projects: vec![gewu_lab::ProjectSpec {
                id: "planned".into(),
                name: "Planned".into(),
                question: "Question".into(),
            }],
            tasks: vec![],
            dependencies: vec![],
        };
        store.create_proposal("plan", "l", &spec, 1).await.unwrap();
        let body = json!({"actor":"human","rationale":"Plan reviewed"});
        assert_eq!(
            write(
                &app,
                "/api/labs/other/proposal/plan/approve",
                &token,
                body.clone()
            )
            .await,
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            write(
                &app,
                "/api/labs/l/proposal/plan/approve",
                &token,
                body.clone()
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            write(
                &app,
                "/api/labs/l/proposal/plan/reject",
                &token,
                body.clone()
            )
            .await,
            StatusCode::CONFLICT
        );
        assert_eq!(
            store.get_project("planned").await.unwrap().unwrap().status,
            gewu_lab::ProjectStatus::Active
        );
        let rejected = gewu_lab::PlanningProposalSpec {
            projects: vec![gewu_lab::ProjectSpec {
                id: "rejected-project".into(),
                name: "Rejected".into(),
                question: "Question".into(),
            }],
            tasks: vec![],
            dependencies: vec![],
        };
        store
            .create_proposal("reject", "l", &rejected, 1)
            .await
            .unwrap();
        assert_eq!(
            write(
                &app,
                "/api/labs/l/proposal/reject/reject",
                &token,
                body.clone()
            )
            .await,
            StatusCode::OK
        );
        assert!(
            store
                .get_project("rejected-project")
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(
            write(&app, "/api/labs/l/task/t/cancel", &token, body).await,
            StatusCode::OK
        );
        assert_eq!(
            store.get_task("t").await.unwrap().unwrap().status,
            gewu_lab::TaskStatus::Cancelled
        );
        let (_, dashboard) = read(&app, "/api/labs/l").await;
        assert_eq!(dashboard["proposal_decisions"].as_array().unwrap().len(), 2);
        assert_eq!(dashboard["decisions"].as_array().unwrap().len(), 3);
        assert!(store.attempts("l").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn approved_mutations_preserve_audit_and_result_review() {
        let (_dir, store, app) = fixture().await;
        let token = token(&app).await;
        let body = json!({"actor":"reviewer","rationale":"Evidence checked"});
        for path in [
            "/api/labs/l/project/p/approve",
            "/api/labs/l/lab/l/pause",
            "/api/labs/l/lab/l/resume",
        ] {
            assert_eq!(
                write(&app, path, &token, body.clone()).await,
                StatusCode::OK
            );
        }
        assert_eq!(
            write(
                &app,
                "/api/labs/l/task/t/priority",
                &token,
                json!({"actor":"reviewer","rationale":"Urgent","priority":42})
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            store
                .get_task("t")
                .await
                .unwrap()
                .unwrap()
                .effective_priority(),
            42
        );
        let now = SystemClock.now();
        let claim = store
            .claim_next("test-worker", now, 300)
            .await
            .unwrap()
            .unwrap();
        store
            .complete_attempt(
                &claim.attempt_id,
                &claim.lease_token,
                &gewu_lab::ExecutionResult {
                    output: "<img src=x onerror=alert(1)>".into(),
                    evidence: "Evidence".into(),
                    uncertainties: "Unverified".into(),
                    artifacts: vec![gewu_lab::ArtifactInput {
                        title: "Artifact".into(),
                        kind: "report".into(),
                        content: "<script>not executable</script>".into(),
                        source: None,
                    }],
                },
                now,
            )
            .await
            .unwrap();
        let (_, value) = read(&app, "/api/labs/l").await;
        assert_eq!(
            value["attempts"][0]["output"],
            "<img src=x onerror=alert(1)>"
        );
        assert_eq!(value["artifacts"][0]["title"], "Artifact");
        assert!(!value.to_string().contains(&claim.lease_token));
        assert_eq!(
            write(
                &app,
                &format!("/api/labs/l/attempt/{}/accept", claim.attempt_id),
                &token,
                body
            )
            .await,
            StatusCode::OK
        );
        assert_eq!(
            store.get_task("t").await.unwrap().unwrap().status,
            gewu_lab::TaskStatus::Completed
        );
        let decisions = store.decisions("l").await.unwrap();
        assert_eq!(decisions.len(), 5);
        assert!(decisions.iter().all(|d| d.actor == "reviewer"));
        assert!(
            decisions
                .iter()
                .any(|d| d.kind == "accept_result" && d.rationale == "Evidence checked")
        );
    }
}
