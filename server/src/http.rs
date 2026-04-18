use std::sync::Arc;

use axum::{Json, extract::State, extract::rejection::JsonRejection, http::StatusCode, response::IntoResponse};
use serde::{Deserialize, Serialize};

use crate::query::QueryEngine;

#[derive(Clone)]
pub struct AppState {
    pub engine: Arc<QueryEngine>,
}

#[derive(Debug, Deserialize)]
pub struct QueryRequest {
    pub sql: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    pub error: String,
}

pub async fn query_handler(
    State(state): State<AppState>,
    request: Result<Json<QueryRequest>, JsonRejection>,
) -> impl IntoResponse {
    let Json(request) = match request {
        Ok(request) => request,
        Err(_) => return invalid_request_response().into_response(),
    };

    match crate::query::execute_query(&state.engine, &request.sql).await {
        Ok(response) => (StatusCode::OK, Json(response)).into_response(),
        Err(crate::query::QueryError::Internal(_)) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorBody {
                error: "internal error".to_string(),
            }),
        )
            .into_response(),
    }
}

pub fn invalid_request_response() -> (StatusCode, Json<ErrorBody>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorBody {
            error: "invalid request".to_string(),
        }),
    )
}
