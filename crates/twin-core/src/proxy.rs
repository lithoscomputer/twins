//! Response-body forwarding shared by the provider proxies.

use std::error::Error;
use std::io;

use async_stream::stream;
use axum::body::{Body, Bytes};
use futures_util::{pin_mut, Stream, StreamExt};

/// Forward each upstream chunk and collect its bytes for recording.
/// The callback runs only when the body is polled to a clean end of stream.
/// An upstream error or a dropped body discards the incomplete capture.
pub fn forward_stream<S, E>(upstream: S, on_complete: impl FnOnce(&[u8]) + Send + 'static) -> Body
where
    S: Stream<Item = Result<Bytes, E>> + Send + 'static,
    E: Error + Send + Sync + 'static,
{
    Body::from_stream(stream! {
        let mut buffer = Vec::new();
        pin_mut!(upstream);
        while let Some(chunk) = upstream.next().await {
            match chunk {
                Ok(chunk) => {
                    buffer.extend_from_slice(&chunk);
                    yield Ok::<_, io::Error>(chunk);
                }
                Err(error) => {
                    tracing::warn!(%error, "upstream stream failed mid-response");
                    yield Err(io::Error::other(error));
                    return;
                }
            }
        }
        on_complete(&buffer);
    })
}
