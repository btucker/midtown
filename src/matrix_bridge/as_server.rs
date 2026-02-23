use axum::{
    Json, Router, extract::Path as AxumPath, http::StatusCode, response::IntoResponse, routing::put,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixTransaction {
    pub events: Vec<MatrixEvent>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatrixEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub sender: String,
    pub room_id: String,
    pub content: Value,
}

#[derive(Debug, Clone)]
pub struct MatrixApplicationService;

impl MatrixApplicationService {
    pub fn router() -> Router {
        Router::new().route(
            "/_matrix/app/v1/transactions/:txn_id",
            put(handle_transaction),
        )
    }
}

pub async fn handle_transaction(
    AxumPath(_txn_id): AxumPath<String>,
    Json(body): Json<MatrixTransaction>,
) -> impl IntoResponse {
    for event in body.events {
        let _ = event;
    }
    StatusCode::OK
}
