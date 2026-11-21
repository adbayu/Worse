use super::*;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[test]
fn sha1_matches_the_reference_vectors() {
    assert_eq!(hex(&sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
    assert_eq!(
        hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        hex(&sha1(b"The quick brown fox jumps over the lazy dog")),
        "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12"
    );
}

#[test]
fn accept_key_matches_rfc_6455_example() {
    assert_eq!(
        accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
        "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
    );
}

#[test]
fn upgrade_detection_requires_both_headers() {
    let mut headers = http::HeaderMap::new();
    assert!(!is_upgrade(&headers));
    headers.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    assert!(!is_upgrade(&headers));
    headers.insert(
        header::CONNECTION,
        HeaderValue::from_static("keep-alive, Upgrade"),
    );
    assert!(is_upgrade(&headers));
}

#[tokio::test]
async fn frames_round_trip_through_a_masked_client_frame() {
    // Masked pong ("hi") is ignored…
    let pong_frame = vec![0x8A, 0x82, 0x01, 0x02, 0x03, 0x04, b'h' ^ 0x01, b'i' ^ 0x02];
    let mut pong_input = pong_frame.as_slice();
    assert!(matches!(
        read_frame(&mut pong_input).await.unwrap(),
        Some(ClientFrame::Other)
    ));

    // …a masked ping is reported with its payload…
    let heartbeat = vec![0x89, 0x80, 0x00, 0x00, 0x00, 0x00];
    let mut heartbeat_input = heartbeat.as_slice();
    match read_frame(&mut heartbeat_input).await.unwrap() {
        Some(ClientFrame::Ping(payload)) => assert_eq!(payload.len(), 0),
        other => panic!("unexpected frame: {other:?}"),
    }

    // …and a close frame ends the stream.
    let close_bytes = vec![0x88, 0x82, 0x00, 0x00, 0x00, 0x00, 0x03, 0xE8];
    let mut close_reader = close_bytes.as_slice();
    assert!(matches!(
        read_frame(&mut close_reader).await.unwrap(),
        Some(ClientFrame::Close)
    ));
}
