use axum::{
    extract::{FromRequest, Request},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use hitomi_core::errors::CommandError;
use serde::de::DeserializeOwned;

/// A failed request, answered with the same `CommandError` the desktop app rejects with
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    error: CommandError,
}

pub type ApiResult<T> = Result<T, ApiError>;

impl ApiError {
    pub fn new(status: StatusCode, error: CommandError) -> Self {
        Self { status, error }
    }

    /// Reported like every command error: logged, and with that shown in the frontend
    pub fn with_status(status: StatusCode, err_title: &str, err: impl Into<anyhow::Error>) -> Self {
        Self::new(status, CommandError::from(err_title, err))
    }
}

/// A command that failed in the core
impl From<CommandError> for ApiError {
    fn from(error: CommandError) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.error)).into_response()
    }
}

/// The JSON arguments of a command
///
/// Like `axum::Json`, but arguments that do not fit answer with a `CommandError`, as every
/// other failure does, instead of a plain text body.
pub struct Args<T>(pub T);

impl<S, T> FromRequest<S> for Args<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(req, state).await {
            Ok(Json(args)) => Ok(Args(args)),
            Err(rejection) => Err(ApiError::with_status(
                StatusCode::BAD_REQUEST,
                "invalid arguments",
                anyhow::anyhow!(rejection.body_text()),
            )),
        }
    }
}
