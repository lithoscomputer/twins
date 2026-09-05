use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;

use axum::body::Bytes;
use futures_util::{poll, stream, StreamExt};
use twin_core::proxy::forward_stream;

#[tokio::test]
async fn chunks_pass_through_before_recording_at_clean_eof() {
    let capture = Arc::new(Mutex::new(None));
    let recorded = capture.clone();
    let reads = Arc::new(AtomicUsize::new(0));
    let upstream_reads = reads.clone();
    let chunks = [
        Bytes::from_static(&[0xff, 0x00]),
        Bytes::from_static(b"\ndata: done\n\n"),
    ];
    let upstream = stream::iter(chunks.clone().map(Ok::<_, io::Error>)).inspect(move |_| {
        upstream_reads.fetch_add(1, Ordering::SeqCst);
    });
    let mut body = forward_stream(upstream, move |bytes| {
        *recorded.lock().expect("capture lock") = Some(bytes.to_vec());
    })
    .into_data_stream();

    assert_eq!(reads.load(Ordering::SeqCst), 0);
    for (index, expected) in chunks.iter().enumerate() {
        assert_eq!(
            &body.next().await.expect("chunk").expect("body bytes"),
            expected
        );
        assert_eq!(reads.load(Ordering::SeqCst), index + 1);
        assert!(capture.lock().expect("capture lock").is_none());
    }
    assert!(body.next().await.is_none());
    assert_eq!(
        capture.lock().expect("capture lock").as_ref(),
        Some(&chunks.concat())
    );
    assert!(body.next().await.is_none());
}

#[tokio::test]
async fn upstream_errors_pass_through_without_recording_or_reading_later_chunks() {
    let reads = Arc::new(AtomicUsize::new(0));
    let upstream_reads = reads.clone();
    let upstream = stream::iter([
        Ok(Bytes::from_static(b"partial")),
        Err(io::Error::other("upstream interrupted")),
        Ok(Bytes::from_static(b"must not be read")),
    ])
    .inspect(move |_| {
        upstream_reads.fetch_add(1, Ordering::SeqCst);
    });
    let mut body =
        forward_stream(upstream, |_| panic!("failed stream must not record")).into_data_stream();

    assert_eq!(body.next().await.expect("chunk").expect("bytes"), "partial");
    let error = body
        .next()
        .await
        .expect("error chunk")
        .expect_err("upstream error");
    assert!(error.to_string().contains("upstream interrupted"));
    assert!(body.next().await.is_none());
    assert_eq!(reads.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn an_open_upstream_delivers_chunks_and_dropping_the_body_does_not_record() {
    let upstream =
        stream::iter([Ok::<_, io::Error>(Bytes::from_static(b"first"))]).chain(stream::pending());
    let mut body =
        forward_stream(upstream, |_| panic!("dropped stream must not record")).into_data_stream();

    match poll!(body.next()) {
        Poll::Ready(Some(Ok(chunk))) => assert_eq!(chunk, "first"),
        _ => panic!("first chunk must be delivered while the upstream is still open"),
    }
    assert!(poll!(body.next()).is_pending());
    drop(body);
}

#[tokio::test]
async fn an_empty_stream_completes_with_an_empty_capture() {
    let capture = Arc::new(Mutex::new(None));
    let recorded = capture.clone();
    let mut body = forward_stream(stream::empty::<Result<Bytes, io::Error>>(), move |bytes| {
        *recorded.lock().expect("capture lock") = Some(bytes.to_vec());
    })
    .into_data_stream();

    assert!(body.next().await.is_none());
    assert_eq!(*capture.lock().expect("capture lock"), Some(Vec::new()));
}
