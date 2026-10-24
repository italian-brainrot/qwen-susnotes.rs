//! Translating vault errors into HTTP responses.
//!
//! The body of every error is [`susnotes_core::api_types::ApiError`], which is
//! the same type the CLI prints, so a client branches on `kind` and never has to
//! parse a message. The mapping is data, not code, and lives in core.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use susnotes_core::{Error, api_types::ApiError};

/// A vault or request error, ready to become an HTTP response.
#[derive(Debug)]
pub struct ApiFailure {
    status: StatusCode,
    body: ApiError,
}

impl ApiFailure {
    /// Map a core error onto a status and a stable `kind`.
    #[must_use]
    pub fn from_core(err: &Error) -> Self {
        let body = ApiError::from_error(err);
        Self {
            status: StatusCode::from_u16(body.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            body,
        }
    }

    /// Pass through an error the core types already described.
    #[must_use]
    pub fn core(body: ApiError) -> Self {
        Self {
            status: StatusCode::from_u16(body.status).unwrap_or(StatusCode::BAD_REQUEST),
            body,
        }
    }

    #[must_use]
    pub fn bad_request(reason: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            body: ApiError {
                status: 400,
                kind: "bad_request".to_owned(),
                message: reason.into(),
            },
        }
    }

    /// A route the API does not have.
    #[must_use]
    pub fn not_found(route: &str) -> Self {
        Self::core(ApiError::not_found(route))
    }
}

impl From<Error> for ApiFailure {
    fn from(err: Error) -> Self {
        Self::from_core(&err)
    }
}

impl IntoResponse for ApiFailure {
    fn into_response(self) -> Response {
        match self.body.kind.as_str() {
            // Expected in normal use: an app opening a note that does not exist
            // yet is the documented flow, not a problem.
            "note_not_found" | "asset_not_found" | "not_found" => {
                tracing::debug!(status = self.body.status, kind = %self.body.kind, "request found nothing");
            }
            // Something a human should see in the log.
            "io" | "format" | "config" | "crypto" | "other" => {
                tracing::warn!(status = self.body.status, kind = %self.body.kind, detail = %self.body.message, "request failed");
            }
            _ => {
                tracing::info!(status = self.body.status, kind = %self.body.kind, "request rejected");
            }
        }
        (self.status, Json(self.body)).into_response()
    }
}

/// Every handler returns this, so the mapping above applies everywhere.
#[allow(dead_code)]
pub type ApiResult<T> = std::result::Result<T, ApiFailure>;
