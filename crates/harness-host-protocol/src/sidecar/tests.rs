use super::*;
use serde_json::json;

fn sample_ready() -> SidecarLine {
    SidecarLine::Ready(ReadyLine {
        v: HOST_BRIDGE_PROTOCOL_VERSION,
        host_pid: 42,
        ws_port: 50_000,
        protocol: 1,
    })
}

fn round_trip(line: &SidecarLine) -> SidecarLine {
    let encoded = serde_json::to_string(line).unwrap();
    SidecarLine::parse(&encoded).unwrap()
}

#[test]
fn ready_line_uses_camel_case_keys_and_round_trips() {
    let line = sample_ready();
    let encoded = serde_json::to_string(&line).unwrap();
    assert_eq!(
        encoded,
        r#"{"t":"ready","v":1,"hostPid":42,"wsPort":50000,"protocol":1}"#
    );
    assert_eq!(round_trip(&line), line);
}

#[test]
fn every_sidecar_variant_round_trips() {
    let lines = [
        sample_ready(),
        SidecarLine::AttachOpened(AttachOpenedLine {
            v: 1,
            request_id: "req-1".to_owned(),
            attachment_id: "att-1".to_owned(),
        }),
        SidecarLine::AttachActive(AttachActiveLine {
            v: 1,
            attachment_id: "att-1".to_owned(),
            message_type: AttachMessageKind::InitLocal,
        }),
        SidecarLine::AttachActive(AttachActiveLine {
            v: 1,
            attachment_id: "att-1".to_owned(),
            message_type: AttachMessageKind::AttachServicePort,
        }),
        SidecarLine::AttachClosed(AttachClosedLine {
            v: 1,
            attachment_id: "att-1".to_owned(),
            reason: CloseReason::Backpressure,
        }),
        SidecarLine::AttachFailed(AttachFailedLine {
            v: 1,
            request_id: "req-1".to_owned(),
            code: SidecarErrorCode::new("attach_rejected"),
            message: "rejected".to_owned(),
        }),
        SidecarLine::Host(HostMessageLine::new(json!({
            "type": "database-startup-state",
            "state": { "phase": "ready" }
        }))),
        SidecarLine::Fatal(FatalLine {
            v: 1,
            code: SidecarErrorCode::new("host_exited"),
            message: "exited".to_owned(),
        }),
        SidecarLine::Log(LogLine {
            v: 1,
            level: LogLevel::Warn,
            message: "diagnostic".to_owned(),
        }),
        SidecarLine::Pong(PongLine {
            v: 1,
            id: "ping-1".to_owned(),
        }),
    ];

    for line in &lines {
        assert_eq!(&round_trip(line), line);
    }
}

#[test]
fn every_close_reason_and_log_level_round_trips() {
    let reasons = [
        CloseReason::WsClosed,
        CloseReason::HostExit,
        CloseReason::Expired,
        CloseReason::Rejected,
        CloseReason::Backpressure,
        CloseReason::Shutdown,
    ];
    for reason in reasons {
        let line = SidecarLine::AttachClosed(AttachClosedLine {
            v: 1,
            attachment_id: "att-1".to_owned(),
            reason,
        });
        assert_eq!(round_trip(&line), line);
    }

    for level in [LogLevel::Info, LogLevel::Warn, LogLevel::Error] {
        let line = SidecarLine::Log(LogLine {
            v: 1,
            level,
            message: "diagnostic".to_owned(),
        });
        assert_eq!(round_trip(&line), line);
    }
}

#[test]
fn every_shell_variant_round_trips() {
    let lines = [
        ShellLine::AttachOpen(AttachOpenLine::new(
            "req-1",
            "att-1",
            "ab".repeat(32),
            30_000,
            json!({ "type": "init-local" }),
        )),
        ShellLine::Host(HostMessageLine::new(json!({
            "type": "database-startup-control",
            "control": "snapshot"
        }))),
        ShellLine::Ping(PingLine::new("ping-1")),
    ];

    for line in &lines {
        let encoded = line.to_line().unwrap();
        let decoded: ShellLine = serde_json::from_str(&encoded).unwrap();
        assert_eq!(&decoded, line);
    }
}

#[test]
fn shell_to_line_serializes_exact_keys() {
    assert_eq!(
        ShellLine::ping("ping-1").to_line().unwrap(),
        r#"{"t":"ping","v":1,"id":"ping-1"}"#
    );
    assert_eq!(
        ShellLine::host(json!({ "type": "dispose" }))
            .to_line()
            .unwrap(),
        r#"{"t":"host","v":1,"message":{"type":"dispose"}}"#
    );
}

#[test]
fn shell_line_from_value_matches_shell_line_to_line() {
    let line = ShellLine::attach_open(AttachOpenLine::new(
        "req-1",
        "att-1",
        "token",
        1_000,
        json!({ "type": "attach-service-port" }),
    ));
    let encoded = line.to_line().unwrap();
    let decoded: ShellLine = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, line);
}

#[test]
fn unknown_kind_is_rejected() {
    assert_eq!(
        SidecarLine::parse(r#"{"v":1,"t":"mystery"}"#),
        Err(ProtocolError::UnknownKind("mystery".to_owned()))
    );
}

#[test]
fn foreign_version_is_rejected_before_kind_dispatch() {
    assert_eq!(
        SidecarLine::parse(r#"{"v":2,"t":"ready"}"#),
        Err(ProtocolError::UnsupportedVersion(2))
    );
    assert_eq!(
        SidecarLine::parse(r#"{"v":70000,"t":"ready"}"#),
        Err(ProtocolError::UnsupportedVersion(u16::MAX))
    );
}

#[test]
fn malformed_lines_are_rejected() {
    assert!(matches!(
        SidecarLine::parse("{"),
        Err(ProtocolError::InvalidJson(_))
    ));
    assert!(matches!(
        SidecarLine::parse("[]"),
        Err(ProtocolError::InvalidJson(_))
    ));
    assert!(matches!(
        SidecarLine::parse(r#"{"v":"1","t":"ready"}"#),
        Err(ProtocolError::InvalidJson(_))
    ));
    assert!(matches!(
        SidecarLine::parse(r#"{"v":1,"t":7}"#),
        Err(ProtocolError::InvalidJson(_))
    ));
    assert_eq!(
        SidecarLine::parse(r#"{"t":"ready"}"#),
        Err(ProtocolError::MissingField("v"))
    );
    assert_eq!(
        SidecarLine::parse(r#"{"v":1}"#),
        Err(ProtocolError::MissingField("t"))
    );
}

#[test]
fn incomplete_payload_is_rejected() {
    assert!(matches!(
        SidecarLine::parse(r#"{"v":1,"t":"ready","hostPid":1}"#),
        Err(ProtocolError::InvalidJson(_))
    ));
}

#[test]
fn oversized_line_is_rejected_before_parsing() {
    let line = "a".repeat(MAX_LINE_BYTES + 1);
    assert_eq!(
        SidecarLine::parse(&line),
        Err(ProtocolError::LineTooLarge(MAX_LINE_BYTES + 1))
    );
}

#[test]
fn sidecar_error_code_keeps_unknown_codes() {
    let known = SidecarErrorCode::new("attach_expired");
    assert!(known.is_known());
    assert_eq!(known.as_str(), "attach_expired");

    let unknown = SidecarErrorCode::new("future_code");
    assert!(!unknown.is_known());
    assert_eq!(unknown.as_str(), "future_code");
    assert_eq!(SIDECAR_ERROR_CODES.len(), 15);
}
