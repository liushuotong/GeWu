use crate::state::{AppState, SessionInfo};
use axum::{
    extract::{Query, State},
    Json,
};
use std::sync::Arc;
use tracing::{info, warn};

#[derive(Debug, serde::Deserialize)]
pub struct CreateSessionRequest {
    pub agent_type: String,
}

#[derive(Debug, serde::Serialize)]
pub struct SessionResponse {
    pub session: SessionInfo,
}

#[derive(Debug, serde::Serialize)]
pub struct SessionsListResponse {
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, serde::Serialize)]
pub struct MessageResponse {
    pub message: String,
}

#[derive(Debug, serde::Deserialize)]
pub struct TokenQuery {
    pub token: Option<String>,
}

pub async fn handle_list_sessions(
    State(app): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
) -> Result<Json<SessionsListResponse>, AppError> {
    info!("handle_list_sessions called with token: {:?}", query.token);
    let auth = app.auth_token.read().await;
    let valid = query.token.as_ref().map(|t| auth.validate(t)).unwrap_or(false);
    info!("Token valid: {}", valid);
    drop(auth);
    
    if !valid {
        warn!("Unauthorized list sessions attempt");
        return Err(AppError::Unauthorized);
    }

    let sessions = app.list_sessions().await;
    info!("Returning {} sessions", sessions.len());
    Ok(Json(SessionsListResponse { sessions }))
}

pub async fn handle_create_session(
    State(app): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
    Json(req): Json<CreateSessionRequest>,
) -> Result<Json<SessionResponse>, AppError> {
    info!("handle_create_session called");
    let auth = app.auth_token.read().await;
    let valid = query.token.as_ref().map(|t| auth.validate(t)).unwrap_or(false);
    info!("Token valid: {}", valid);
    drop(auth);
    
    if !valid {
        warn!("Unauthorized create session attempt");
        return Err(AppError::Unauthorized);
    }

    info!("Creating session with agent_type: {}", req.agent_type);
    let session = app.create_session(req.agent_type).await?;
    info!("Session created: {}", session.id);
    Ok(Json(SessionResponse { session }))
}

pub async fn handle_delete_session(
    State(app): State<Arc<AppState>>,
    Query(query): Query<TokenQuery>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<Json<MessageResponse>, AppError> {
    let auth = app.auth_token.read().await;
    let valid = query.token.as_ref().map(|t| auth.validate(t)).unwrap_or(false);
    drop(auth);
    
    if !valid {
        return Err(AppError::Unauthorized);
    }

    app.delete_session(&id).await?;
    Ok(Json(MessageResponse {
        message: "Session deleted".to_string(),
    }))
}

#[derive(Debug)]
pub enum AppError {
    Unauthorized,
    SessionNotFound,
    InternalError(String),
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        AppError::InternalError(e.to_string())
    }
}

impl axum::response::IntoResponse for AppError {
    fn into_response(self) -> axum::response::Response {
        match self {
            AppError::Unauthorized => {
                (axum::http::StatusCode::UNAUTHORIZED, "Unauthorized").into_response()
            }
            AppError::SessionNotFound => {
                (axum::http::StatusCode::NOT_FOUND, "Session not found").into_response()
            }
            AppError::InternalError(msg) => {
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, msg).into_response()
            }
        }
    }
}
