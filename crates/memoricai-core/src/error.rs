//! Typed error with HTTP-status mapping, shared across all layers.

use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Error)]
pub enum Error {
    #[error("not found: {0}")]
    NotFound(String),
    #[error("bad request: {0}")]
    BadRequest(String),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("forbidden: {0}")]
    Forbidden(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("rate limited")]
    RateLimited,
    #[error("payment required: {0}")]
    PaymentRequired(String),
    #[error("model/upstream error: {0}")]
    Model(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// HTTP status code for this error.
    pub fn status(&self) -> u16 {
        match self {
            Error::BadRequest(_) => 400,
            Error::Unauthorized(_) => 401,
            Error::PaymentRequired(_) => 402,
            Error::Forbidden(_) => 403,
            Error::NotFound(_) => 404,
            Error::Conflict(_) => 409,
            Error::RateLimited => 429,
            Error::Model(_) => 502,
            Error::Database(_) | Error::Internal(_) => 500,
        }
    }

    /// Stable machine code for the JSON error envelope's `error` field.
    pub fn code(&self) -> &'static str {
        match self {
            Error::BadRequest(_) => "bad_request",
            Error::Unauthorized(_) => "unauthorized",
            Error::PaymentRequired(_) => "payment_required",
            Error::Forbidden(_) => "forbidden",
            Error::NotFound(_) => "not_found",
            Error::Conflict(_) => "conflict",
            Error::RateLimited => "rate_limited",
            Error::Model(_) => "upstream_error",
            Error::Database(_) => "database_error",
            Error::Internal(_) => "internal_error",
        }
    }
}
