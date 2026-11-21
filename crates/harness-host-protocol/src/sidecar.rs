//! NDJSON line types exchanged with the Node sidecar.
//!
//! Every line is one JSON object with `v = 1` and a `t` discriminator. The
//! sidecar writes `SidecarLine` values to its stdout; the shell writes
//! `ShellLine` values to the sidecar's stdin. Parsing rejects oversized lines,
//! foreign versions, unknown kinds, and non-object JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{ProtocolError, HOST_BRIDGE_PROTOCOL_VERSION, MAX_LINE_BYTES};

/// Error codes the sidecar may report.
pub const SIDECAR_ERROR_CODES: [&str; 15] = [
    "bad_line",
    "unsupported_version",
    "unknown_kind",
    "line_too_large",
    "attach_duplicate",
    "attach_unknown",
    "attach_expired",
    "attach_rejected",
    "ws_origin_rejected",
    "ws_token_rejected",
    "ws_backpressure",
    "host_import_failed",
    "host_exited",
    "stdin_closed",
    "internal",
];

/// A machine-readable sidecar error code.
///
/// The newtype keeps unknown codes intact so a newer sidecar can report a
/// failure without breaking an older shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SidecarErrorCode(String);

impl SidecarErrorCode {
    /// Wraps a raw code string.
    pub fn new(code: impl Into<String>) -> Self {
        Self(code.into())
    }

    /// Returns the raw code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }

    /// Returns whether the code belongs to the documented set.
    #[must_use]
    pub fn is_known(&self) -> bool {
        SIDECAR_ERROR_CODES.contains(&self.as_str())
    }
}

/// Which Host message type the sidecar injected the virtual port for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttachMessageKind {
    /// First attachment of the Host process.
    InitLocal,
    /// Attachment of an already initialized Host, for example after a reload.
    AttachServicePort,
}

/// Why the sidecar closed an attachment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CloseReason {
    /// The WebSocket closed.
    WsClosed,
    /// The Host process exited.
    HostExit,
    /// The pending attachment token expired before a client connected.
    Expired,
    /// The sidecar rejected the attachment request.
    Rejected,
    /// The outbound buffer exceeded the configured threshold.
    Backpressure,
    /// The sidecar is shutting down.
    Shutdown,
}

/// Level of a diagnostics line emitted by the sidecar launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Informational diagnostics.
    Info,
    /// Recoverable diagnostics.
    Warn,
    /// Failure diagnostics.
    Error,
}

/// `ready`: the launcher imported the Host and bound the WebSocket server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadyLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Operating-system pid of the sidecar launcher.
    pub host_pid: u32,
    /// Loopback port of the attachment WebSocket server.
    pub ws_port: u16,
    /// Protocol version the sidecar speaks.
    pub protocol: u16,
}

/// `attach.opened`: the pending attachment is registered and waiting for a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachOpenedLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Correlation id of the originating `attach.open` line.
    pub request_id: String,
    /// Identifier of the attachment.
    pub attachment_id: String,
}

/// `attach.active`: the Host received the injected virtual port.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachActiveLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Identifier of the attachment.
    pub attachment_id: String,
    /// Host message type that carried the port.
    pub message_type: AttachMessageKind,
}

/// `attach.closed`: the attachment ended for a documented reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachClosedLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Identifier of the attachment.
    pub attachment_id: String,
    /// Why the attachment ended.
    pub reason: CloseReason,
}

/// `attach.failed`: the sidecar could not open or serve the attachment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachFailedLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Correlation id of the originating `attach.open` line.
    pub request_id: String,
    /// Machine-readable failure code.
    pub code: SidecarErrorCode,
    /// Human-readable failure message.
    pub message: String,
}

/// `host`: one unmodified object the Host passed to `parentPort.postMessage`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostMessageLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// The Host message exactly as the Host emitted it.
    pub message: Value,
}

impl HostMessageLine {
    /// Wraps a Host message with the current bridge version.
    #[must_use]
    pub fn new(message: Value) -> Self {
        Self {
            v: HOST_BRIDGE_PROTOCOL_VERSION,
            message,
        }
    }
}

/// `fatal`: the sidecar cannot continue and is about to exit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FatalLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Machine-readable failure code.
    pub code: SidecarErrorCode,
    /// Human-readable failure message.
    pub message: String,
}

/// `log`: launcher diagnostics that are not Host output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Diagnostics level.
    pub level: LogLevel,
    /// Diagnostics message.
    pub message: String,
}

/// `pong`: reply to a shell `ping`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PongLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Identifier copied from the `ping`.
    pub id: String,
}

/// `attach.open`: ask the sidecar to register an attachment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachOpenLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Correlation id echoed by `attach.opened` or `attach.failed`.
    pub request_id: String,
    /// Identifier generated by the shell for this attachment.
    pub attachment_id: String,
    /// Single-use 64 hex character token the WebSocket client must present.
    pub token: String,
    /// Lifetime of the pending attachment.
    pub ttl_ms: u64,
    /// Complete `init-local` or `attach-service-port` message without a port.
    pub host_message: Value,
}

impl AttachOpenLine {
    /// Builds an `attach.open` line with the current bridge version.
    #[must_use]
    pub fn new(
        request_id: impl Into<String>,
        attachment_id: impl Into<String>,
        token: impl Into<String>,
        ttl_ms: u64,
        host_message: Value,
    ) -> Self {
        Self {
            v: HOST_BRIDGE_PROTOCOL_VERSION,
            request_id: request_id.into(),
            attachment_id: attachment_id.into(),
            token: token.into(),
            ttl_ms,
            host_message,
        }
    }
}

/// `ping`: liveness probe answered by `pong`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PingLine {
    /// Bridge version; always [`HOST_BRIDGE_PROTOCOL_VERSION`].
    pub v: u16,
    /// Correlation id echoed by `pong`.
    pub id: String,
}

impl PingLine {
    /// Builds a `ping` line with the current bridge version.
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            v: HOST_BRIDGE_PROTOCOL_VERSION,
            id: id.into(),
        }
    }
}

/// A line written by the sidecar to its stdout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum SidecarLine {
    /// The launcher is ready to accept attachments.
    #[serde(rename = "ready")]
    Ready(ReadyLine),
    /// A pending attachment was registered.
    #[serde(rename = "attach.opened")]
    AttachOpened(AttachOpenedLine),
    /// The Host received an attachment's virtual port.
    #[serde(rename = "attach.active")]
    AttachActive(AttachActiveLine),
    /// An attachment ended.
    #[serde(rename = "attach.closed")]
    AttachClosed(AttachClosedLine),
    /// An attachment could not be created or served.
    #[serde(rename = "attach.failed")]
    AttachFailed(AttachFailedLine),
    /// One Host message.
    #[serde(rename = "host")]
    Host(HostMessageLine),
    /// The launcher cannot continue.
    #[serde(rename = "fatal")]
    Fatal(FatalLine),
    /// Launcher diagnostics.
    #[serde(rename = "log")]
    Log(LogLine),
    /// Reply to a `ping`.
    #[serde(rename = "pong")]
    Pong(PongLine),
}

impl SidecarLine {
    /// Parses one NDJSON line emitted by the sidecar.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::LineTooLarge`] when the line exceeds
    /// [`MAX_LINE_BYTES`], [`ProtocolError::InvalidJson`] when the line is not
    /// a JSON object of the expected shape, [`ProtocolError::MissingField`]
    /// when the `v` or `t` envelope fields are absent,
    /// [`ProtocolError::UnsupportedVersion`] when `v` is not
    /// [`HOST_BRIDGE_PROTOCOL_VERSION`], and [`ProtocolError::UnknownKind`] for
    /// an unrecognized `t`.
    pub fn parse(line: &str) -> Result<Self, ProtocolError> {
        let value = parse_envelope(line)?;
        let kind = line_kind(&value)?;
        if !is_sidecar_kind(kind) {
            return Err(ProtocolError::UnknownKind(kind.to_owned()));
        }
        serde_json::from_value(value).map_err(|error| ProtocolError::InvalidJson(error.to_string()))
    }
}

/// A line the shell writes to the sidecar's stdin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t")]
pub enum ShellLine {
    /// Register an attachment and hand the Host a virtual port.
    #[serde(rename = "attach.open")]
    AttachOpen(AttachOpenLine),
    /// One Host message.
    #[serde(rename = "host")]
    Host(HostMessageLine),
    /// Liveness probe.
    #[serde(rename = "ping")]
    Ping(PingLine),
}

impl ShellLine {
    /// Serializes the line as one NDJSON record, without a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::InvalidJson`] if the payload cannot be encoded
    /// and [`ProtocolError::LineTooLarge`] if the encoded line exceeds
    /// [`MAX_LINE_BYTES`].
    pub fn to_line(&self) -> Result<String, ProtocolError> {
        let line = serde_json::to_string(self)
            .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(ProtocolError::LineTooLarge(line.len()));
        }
        Ok(line)
    }

    /// Builds an `attach.open` line.
    #[must_use]
    pub fn attach_open(line: AttachOpenLine) -> Self {
        Self::AttachOpen(line)
    }

    /// Builds a `host` line from a Host message.
    #[must_use]
    pub fn host(message: Value) -> Self {
        Self::Host(HostMessageLine::new(message))
    }

    /// Builds a `ping` line.
    #[must_use]
    pub fn ping(id: impl Into<String>) -> Self {
        Self::Ping(PingLine::new(id))
    }
}

fn parse_envelope(line: &str) -> Result<Value, ProtocolError> {
    if line.len() > MAX_LINE_BYTES {
        return Err(ProtocolError::LineTooLarge(line.len()));
    }
    let value: Value = serde_json::from_str(line)
        .map_err(|error| ProtocolError::InvalidJson(error.to_string()))?;
    if !value.is_object() {
        return Err(ProtocolError::InvalidJson(
            "line must be a JSON object".to_owned(),
        ));
    }
    let version = value.get("v").ok_or(ProtocolError::MissingField("v"))?;
    let version = version.as_u64().ok_or_else(|| {
        ProtocolError::InvalidJson("field `v` must be an unsigned integer".to_owned())
    })?;
    if version != u64::from(HOST_BRIDGE_PROTOCOL_VERSION) {
        let version = u16::try_from(version).unwrap_or(u16::MAX);
        return Err(ProtocolError::UnsupportedVersion(version));
    }
    Ok(value)
}

fn line_kind(value: &Value) -> Result<&str, ProtocolError> {
    let kind = value.get("t").ok_or(ProtocolError::MissingField("t"))?;
    kind.as_str()
        .ok_or_else(|| ProtocolError::InvalidJson("field `t` must be a string".to_owned()))
}

fn is_sidecar_kind(kind: &str) -> bool {
    matches!(
        kind,
        "ready"
            | "attach.opened"
            | "attach.active"
            | "attach.closed"
            | "attach.failed"
            | "host"
            | "fatal"
            | "log"
            | "pong"
    )
}

#[cfg(test)]
mod tests;
