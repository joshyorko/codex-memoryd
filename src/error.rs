//! Stable error model for the provider (SPEC §14).
//!
//! Every fallible provider operation returns [`Error`], which carries a stable
//! machine-readable [`ErrorCode`] plus a human-readable message. The HTTP layer
//! maps these onto the response envelope and an appropriate status code.

use serde::Serialize;
use std::fmt;

/// Stable error codes. These are part of the provider contract and MUST remain
/// stable across minor versions (SPEC §14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidRequest,
    MissingProfile,
    MissingWorkspace,
    UnknownProfile,
    UnknownWorkspace,
    StorageUnavailable,
    PolicyDenied,
    SecretDetected,
    ProfileBoundaryDenied,
    SyncSourceInvalid,
    NotFound,
    UnsupportedVersion,
    InternalError,
}

impl ErrorCode {
    /// The string form used on the wire and in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::MissingProfile => "missing_profile",
            ErrorCode::MissingWorkspace => "missing_workspace",
            ErrorCode::UnknownProfile => "unknown_profile",
            ErrorCode::UnknownWorkspace => "unknown_workspace",
            ErrorCode::StorageUnavailable => "storage_unavailable",
            ErrorCode::PolicyDenied => "policy_denied",
            ErrorCode::SecretDetected => "secret_detected",
            ErrorCode::ProfileBoundaryDenied => "profile_boundary_denied",
            ErrorCode::SyncSourceInvalid => "sync_source_invalid",
            ErrorCode::NotFound => "not_found",
            ErrorCode::UnsupportedVersion => "unsupported_version",
            ErrorCode::InternalError => "internal_error",
        }
    }

    /// Recommended HTTP status for this error code.
    pub fn http_status(self) -> u16 {
        match self {
            ErrorCode::InvalidRequest | ErrorCode::MissingProfile | ErrorCode::MissingWorkspace => {
                400
            }
            ErrorCode::UnknownProfile | ErrorCode::UnknownWorkspace | ErrorCode::NotFound => 404,
            ErrorCode::PolicyDenied
            | ErrorCode::SecretDetected
            | ErrorCode::ProfileBoundaryDenied => 422,
            ErrorCode::SyncSourceInvalid => 400,
            ErrorCode::UnsupportedVersion => 400,
            ErrorCode::StorageUnavailable => 503,
            ErrorCode::InternalError => 500,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The provider error type. Carries a stable code and a message.
#[derive(Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidRequest, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::NotFound, message)
    }

    pub fn storage(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::StorageUnavailable, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InternalError, message)
    }

    pub fn policy(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PolicyDenied, message)
    }

    pub fn secret(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::SecretDetected, message)
    }

    pub fn profile_boundary(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::ProfileBoundaryDenied, message)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)
    }
}

impl std::error::Error for Error {}

/// SQLite errors collapse to `storage_unavailable` unless they are an explicit
/// "not found", which is mapped at the call site.
impl From<rusqlite::Error> for Error {
    fn from(err: rusqlite::Error) -> Self {
        Error::storage(format!("sqlite: {err}"))
    }
}

impl From<r2d2::Error> for Error {
    fn from(err: r2d2::Error) -> Self {
        Error::storage(format!("connection pool: {err}"))
    }
}

impl From<serde_json::Error> for Error {
    fn from(err: serde_json::Error) -> Self {
        Error::invalid_request(format!("json: {err}"))
    }
}

impl From<std::io::Error> for Error {
    fn from(err: std::io::Error) -> Self {
        Error::new(ErrorCode::SyncSourceInvalid, format!("io: {err}"))
    }
}

/// Convenience alias used across the crate.
pub type Result<T> = std::result::Result<T, Error>;
