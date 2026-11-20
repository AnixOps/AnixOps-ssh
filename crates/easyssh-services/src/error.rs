//! What can go wrong when reading a service. Messages are for the user and
//! never contain a credential.

use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    /// A setting is missing or not acceptable; nothing was sent.
    Invalid(String),
    /// The service could not be reached or the reply was cut off.
    Network(String),
    /// 401: the credential was refused.
    Unauthorized(String),
    /// 403: the credential is known but not allowed to do this.
    Forbidden(String),
    /// 429: too many requests; `retry_after` is in seconds when the service said.
    RateLimited { retry_after: Option<u64> },
    /// Any other status.
    Status(u16, String),
    /// The reply is not what the service's API documents.
    Protocol(String),
    /// The sealed credential could not be read or written.
    Secret(String),
}

impl ServiceError {
    /// A short stable code for the UI.
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::Invalid(_) => "invalid_settings",
            ServiceError::Network(_) => "network",
            ServiceError::Unauthorized(_) => "unauthorized",
            ServiceError::Forbidden(_) => "forbidden",
            ServiceError::RateLimited { .. } => "rate_limited",
            ServiceError::Status(..) => "service_error",
            ServiceError::Protocol(_) => "unexpected_reply",
            ServiceError::Secret(_) => "secret",
        }
    }
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServiceError::Invalid(m)
            | ServiceError::Network(m)
            | ServiceError::Unauthorized(m)
            | ServiceError::Forbidden(m)
            | ServiceError::Protocol(m)
            | ServiceError::Secret(m) => f.write_str(m),
            ServiceError::RateLimited {
                retry_after: Some(seconds),
            } => write!(
                f,
                "The service is limiting requests from this address. Try again in {seconds} seconds."
            ),
            ServiceError::RateLimited { retry_after: None } => {
                f.write_str("The service is limiting requests from this address. Try again later.")
            }
            ServiceError::Status(status, m) if m.is_empty() => {
                write!(f, "The service answered {status}.")
            }
            ServiceError::Status(status, m) => write!(f, "The service answered {status}: {m}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<easyssh_store::StoreError> for ServiceError {
    fn from(error: easyssh_store::StoreError) -> Self {
        ServiceError::Secret(error.to_string())
    }
}

pub type Result<T> = std::result::Result<T, ServiceError>;
