//! API handlers

use crate::models::*;
use axum::{
    extract::Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Upsert handler
pub async fn upsert_handler(Json(req): Json<UpsertRequest>) -> Result<impl IntoResponse, ApiError> {
    // TODO: Implement upsert logic
    Ok(StatusCode::OK)
}

/// Query handler
pub async fn query_handler(Json(req): Json<QueryRequest>) -> Result<Json<QueryResponse>, ApiError> {
    // TODO: Implement query logic
    let response = QueryResponse {
        matches: Vec::new(),
    };

    Ok(Json(response))
}

/// API error type
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(ErrorResponse {
            error: self.status.to_string(),
            message: self.message,
        });

        (self.status, body).into_response()
    }
}

impl From<elacsym_core::Error> for ApiError {
    fn from(err: elacsym_core::Error) -> Self {
        ApiError {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: err.to_string(),
        }
    }
}
