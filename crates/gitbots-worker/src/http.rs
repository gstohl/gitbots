//! Errors and responses. Every error body is `{"error": "..."}`.

use gitbots_cloud::source::SourceError;
use gitbots_core::LookupError;
use serde::Serialize;
use serde::de::DeserializeOwned;
use worker::{Headers, Request, Response};

use crate::artifacts::ArtifactsError;

#[derive(Debug)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: u16, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
    }

    pub fn bad_request(m: impl Into<String>) -> Self {
        Self::new(400, m)
    }

    pub fn unauthorized(m: impl Into<String>) -> Self {
        Self::new(401, m)
    }

    pub fn not_found(m: impl Into<String>) -> Self {
        Self::new(404, m)
    }

    pub fn conflict(m: impl Into<String>) -> Self {
        Self::new(409, m)
    }

    pub fn unprocessable(m: impl Into<String>) -> Self {
        Self::new(422, m)
    }

    pub fn internal(m: impl Into<String>) -> Self {
        Self::new(500, m)
    }

    pub fn into_response(self) -> worker::Result<Response> {
        if self.status >= 500 {
            worker::console_error!("{} {}", self.status, self.message);
        }
        json_response(self.status, &serde_json::json!({ "error": self.message }))
    }
}

impl From<worker::Error> for ApiError {
    fn from(e: worker::Error) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::internal(e.to_string())
    }
}

impl From<ArtifactsError> for ApiError {
    fn from(e: ArtifactsError) -> Self {
        let status = match e.code.as_deref() {
            Some("NOT_FOUND") => 404,
            Some(
                "ALREADY_EXISTS" | "CREATE_IN_PROGRESS" | "FORK_IN_PROGRESS" | "IMPORT_IN_PROGRESS",
            ) => 409,
            Some("INVALID_INPUT" | "INVALID_REPO_NAME" | "INVALID_TTL" | "INVALID_URL") => 422,
            _ => 502,
        };
        Self::new(status, e.to_string())
    }
}

impl From<LookupError> for ApiError {
    fn from(e: LookupError) -> Self {
        let status = match e {
            LookupError::NotFound { .. } => 404,
            LookupError::Ambiguous { .. } => 409,
        };
        Self::new(status, e.to_string())
    }
}

impl From<SourceError> for ApiError {
    fn from(e: SourceError) -> Self {
        match e {
            SourceError::NotFound { .. } => Self::not_found(format!("{e} (not pushed yet?)")),
            SourceError::Backend(m) => Self::new(502, m),
        }
    }
}

fn with_type(resp: Response, content_type: &str) -> worker::Result<Response> {
    let headers = Headers::new();
    headers.set("content-type", content_type)?;
    headers.set("cache-control", "no-store")?;
    Ok(resp.with_headers(headers))
}

pub fn json_response<T: Serialize + ?Sized>(status: u16, body: &T) -> worker::Result<Response> {
    let text = serde_json::to_string(body).map_err(|e| worker::Error::RustError(e.to_string()))?;
    with_type(Response::ok(text)?.with_status(status), "application/json")
}

pub fn ok<T: Serialize + ?Sized>(body: &T) -> ApiResult<Response> {
    Ok(json_response(200, body)?)
}

pub fn text_response(body: String) -> ApiResult<Response> {
    Ok(with_type(Response::ok(body)?, "text/plain; charset=utf-8")?)
}

/// Parses a JSON request body: 400 when it is not JSON, 422 when it is JSON
/// of the wrong shape (as axum's `Json` extractor answers in `gitbots ui`).
pub async fn body<T: DeserializeOwned>(req: &mut Request) -> ApiResult<T> {
    let text = req.text().await.map_err(|e| ApiError::bad_request(e.to_string()))?;
    let text = if text.trim().is_empty() { "{}" } else { text.as_str() };
    serde_json::from_str(text).map_err(|e| {
        let status = if e.is_data() { 422 } else { 400 };
        ApiError::new(status, format!("invalid body: {e}"))
    })
}

/// The bearer token of the request, if any.
pub fn bearer(req: &Request) -> Option<String> {
    let header = req.headers().get("authorization").ok().flatten()?;
    gitbots_cloud::naming::bearer(&header).map(str::to_owned)
}
