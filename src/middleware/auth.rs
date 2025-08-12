use axum::{http::StatusCode, middleware::Next};
use axum::extract::State;

use crate::state::AppState;

pub async fn auth_middleware(
    State(state): State<AppState>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(pw) = &state.cfg.password {
        let hdr = req
            .headers()
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|h| h.to_str().ok());
        match hdr {
            Some(val) if val == pw => {}
            _ => return Err(StatusCode::UNAUTHORIZED),
        }
    }
    Ok(next.run(req).await)
}
