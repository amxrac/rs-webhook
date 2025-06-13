use axum::{
    error_handling::HandleErrorLayer, extract::State, http::{header, HeaderMap, StatusCode}, response::{IntoResponse, Json}, routing::{get, post}, Router
};
use serde_json::{Value, json};
use serde::{Deserialize, Serialize};
use tower::{BoxError, ServiceBuilder};
use tower_http::trace::TraceLayer;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use std::{
    time::Duration
};
use sqlx::sqlite::SqlitePool;
use chrono::Utc;
use std::env;
use dotenv::dotenv;
use sha2::Sha256;
use hmac::{Hmac, Mac};
use axum::response::Response;


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();
    
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| {
                format!("{}=debug,tower_http=debug", env!("CARGO_CRATE_NAME")).into()
            }),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let pool = SqlitePool::connect("sqlite:webhooks.db?mode=rwc").await?;
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS webhook_events (
            id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL,
            payload TEXT,
            received_at DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP
        )
        "#
    ).execute(&pool)
    .await?;

    let app = Router::new()
        .route("/webhook", post(webhook))
        .route("/events", get(events))
        .layer(
            ServiceBuilder::new()
                .layer(HandleErrorLayer::new(|error: BoxError| async move {
                    if error.is::<tower::timeout::error::Elapsed>() {
                        Ok(StatusCode::REQUEST_TIMEOUT)
                    } else {
                        Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Unhandled internal error: {error}"),
                        ))
                    }
                }))
                .timeout(Duration::from_secs(10))
                .layer(TraceLayer::new_for_http())
                .into_inner(),
        )
        .with_state(pool);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    tracing::debug!("listening on {}", listener.local_addr().unwrap());
    axum::serve(listener, app).await?;
    Ok(())
}

async fn webhook(State(pool): State<SqlitePool>, headers: HeaderMap, body: String) -> Response {
    let secret = env::var("WEBHOOK_SECRET").expect("WEBHOOK_SECRET not set");
    let signature = match headers.get("x-hub-signature-256")
        .and_then(|v| v.to_str().ok()) {
            Some(sig) => sig,
            None => return (StatusCode::BAD_REQUEST, Json(json!({"error": "missing signature"}))).into_response()
        };

    
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body.as_bytes());
    let expected = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));
    
    if signature != expected {
        return (StatusCode::UNAUTHORIZED, Json(json!({"error": "Invalid signature"}))).into_response();
    }

    sqlx::query(
        r#"
        INSERT INTO webhook_events (payload, received_at) VALUES (?, ?)
        "#
    )
    .bind(&body)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(&pool)
    .await
    .unwrap();
    
    info!("request processed successfully");
    (StatusCode::OK, Json(json!({"status": "payload received"}))).into_response()
}

async fn events(State(pool): State<SqlitePool>) -> impl IntoResponse {
    let events = sqlx::query_as::<_, (i64, String, String)>(
        "SELECT id, payload, received_at FROM webhook_events ORDER BY received_at DESC LIMIT 50"
    )
    .fetch_all(&pool)
    .await
    .unwrap();

    let formatted: Vec<_> = events.into_iter().map(|(id, payload, received_at)| {
        json!({
            "id": id,
            "received_at": received_at,
            "payload": payload.parse::<Value>().unwrap_or(json!({}))
        })
    }).collect();

    
    info!("request processed successfully");
    Json(json!({"events": formatted, "count": formatted.len()}))
}

// secret key: webhooksecretkey9876543210