use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use easyssh_sync::wire::{ErrorBody, ErrorCode};
use serde::de::DeserializeOwned;

#[derive(Debug)]
pub struct ApiError {
    pub status: StatusCode,
    pub code: ErrorCode,
    pub message: String,
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
            retry_after: None,
        }
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, ErrorCode::BadRequest, message)
    }

    pub fn unauthorized(message: impl Into<String>) -> Self {
        Self::new(StatusCode::UNAUTHORIZED, ErrorCode::Unauthorized, message)
    }

    pub fn forbidden(message: impl Into<String>) -> Self {
        Self::new(StatusCode::FORBIDDEN, ErrorCode::Forbidden, message)
    }

    pub fn coded(status: StatusCode, code: ErrorCode, message: impl Into<String>) -> Self {
        Self::new(status, code, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, ErrorCode::NotFound, message)
    }

    pub fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, ErrorCode::Conflict, message)
    }

    pub fn too_large(message: impl Into<String>) -> Self {
        Self::new(StatusCode::PAYLOAD_TOO_LARGE, ErrorCode::TooLarge, message)
    }

    pub fn rate_limited(retry_after: u64) -> Self {
        Self {
            retry_after: Some(retry_after),
            ..Self::new(
                StatusCode::TOO_MANY_REQUESTS,
                ErrorCode::RateLimited,
                "too many requests",
            )
        }
    }

    /// Internal errors never leak details to the client; they are logged.
    pub fn internal(message: &str) -> Self {
        tracing::error!(error = message, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            ErrorCode::Internal,
            "internal error",
        )
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(error: rusqlite::Error) -> Self {
        ApiError::internal(&format!("database: {error}"))
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response();
        if let Some(seconds) = self.retry_after {
            if let Ok(value) = HeaderValue::from_str(&seconds.to_string()) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

/// `Json` with this crate's error body for malformed or oversized input.
pub struct ApiJson<T>(pub T);

impl<T, S> FromRequest<S> for ApiJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(value)) => Ok(ApiJson(value)),
            Err(JsonRejection::BytesRejection(rejection))
                if rejection.status() == StatusCode::PAYLOAD_TOO_LARGE =>
            {
                Err(ApiError::too_large("request body too large"))
            }
            Err(_) => Err(ApiError::bad_request("malformed JSON body")),
        }
    }
}
