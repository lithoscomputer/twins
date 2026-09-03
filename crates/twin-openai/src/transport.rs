use std::io;
use std::time::Duration;

use async_stream::stream;
use axum::body::{Body, Bytes};
use axum::http::{header, HeaderValue, Response, StatusCode};
use serde::Deserialize;
use tokio::time::sleep;

/// One response-body action in a raw scenario.
#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RawChunk {
    /// Writes UTF-8 text to the response body.
    Text {
        text: String,
        #[serde(default)]
        delay_ms: u64,
    },
    /// Writes the byte values exactly as supplied.
    Bytes {
        bytes: Vec<u8>,
        #[serde(default)]
        delay_ms: u64,
    },
    /// Fails the response body and closes the connection.
    Error {
        message: String,
        #[serde(default)]
        delay_ms: u64,
    },
}

#[derive(Clone, Debug)]
pub struct RawOutcome {
    pub status: StatusCode,
    pub content_type: Option<String>,
    pub chunks: Vec<RawChunk>,
    pub delay_before_headers_ms: u64,
}

/// Builds a response from exact, independently delayed body actions.
pub async fn raw_response(outcome: RawOutcome) -> Response<Body> {
    if outcome.delay_before_headers_ms > 0 {
        sleep(Duration::from_millis(outcome.delay_before_headers_ms)).await;
    }

    let body = Body::from_stream(stream! {
        for chunk in outcome.chunks {
            match chunk {
                RawChunk::Text { text, delay_ms } => {
                    delay(delay_ms).await;
                    yield Ok::<_, io::Error>(Bytes::from(text));
                }
                RawChunk::Bytes { bytes, delay_ms } => {
                    delay(delay_ms).await;
                    yield Ok::<_, io::Error>(Bytes::from(bytes));
                }
                RawChunk::Error { message, delay_ms } => {
                    delay(delay_ms).await;
                    yield Err(io::Error::new(io::ErrorKind::ConnectionReset, message));
                    break;
                }
            }
        }
    });

    let mut response = Response::new(body);
    *response.status_mut() = outcome.status;
    if let Some(content_type) = outcome.content_type {
        if let Ok(value) = HeaderValue::try_from(content_type) {
            response.headers_mut().insert(header::CONTENT_TYPE, value);
        }
    }
    response
}

async fn delay(delay_ms: u64) {
    if delay_ms > 0 {
        sleep(Duration::from_millis(delay_ms)).await;
    }
}
