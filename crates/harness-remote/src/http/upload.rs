use super::*;

/// `POST /api/v1/upload`: raw body saved under `state_dir/remote-uploads/<yyyymmdd>/`, returning the
/// absolute path so the phone can attach it as `ContextItem::File`.
pub(super) async fn upload(
    inner: &Arc<Inner>,
    peer: SocketAddr,
    req: Request<Incoming>,
) -> Response<Body> {
    if let Err(status) = authenticate(inner, peer, req.headers()) {
        return empty(status);
    }
    if !origin_ok(inner, req.headers()) {
        return empty(StatusCode::FORBIDDEN);
    }
    let requested = header_str(req.headers(), "x-file-name").unwrap_or("file");
    let name = sanitize_file_name(requested);
    let Some(body) = read_body(req, MAX_UPLOAD_BODY).await else {
        return empty(StatusCode::PAYLOAD_TOO_LARGE);
    };
    if body.is_empty() {
        return json_response(
            StatusCode::OK,
            &json!({ "ok": false, "error": "empty file" }),
        );
    }
    let dir = inner.state_dir.join("remote-uploads").join(utc_day());
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        return json_response(
            StatusCode::OK,
            &json!({ "ok": false, "error": format!("cannot create upload dir: {error}") }),
        );
    }
    let path = dir.join(format!("{}-{name}", random_hex(8)));
    if let Err(error) = tokio::fs::write(&path, &body).await {
        return json_response(
            StatusCode::OK,
            &json!({ "ok": false, "error": format!("cannot write upload: {error}") }),
        );
    }
    let absolute = std::path::absolute(&path).unwrap_or(path);
    json_response(
        StatusCode::OK,
        &json!({ "ok": true, "path": absolute.to_string_lossy() }),
    )
}

/// Basename only, no `..`, no separators, no control chars, at most 100 chars.
pub(super) fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or_default();
    let mut name: String = base
        .chars()
        .filter(|ch| !ch.is_control() && *ch != '{' && *ch != '}')
        .collect();
    while name.contains("..") {
        name = name.replace("..", ".");
    }
    let trimmed = name.trim_matches(|ch: char| ch == '.' || ch.is_whitespace());
    let clipped: String = trimmed.chars().take(100).collect();
    if clipped.is_empty() {
        "file".to_owned()
    } else {
        clipped
    }
}

/// UTC `yyyymmdd` without pulling in a date crate.
pub(super) fn utc_day() -> String {
    let secs = crate::auth::now_ms() / 1000;
    let (year, month, day) = civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0));
    format!("{year:04}{month:02}{day:02}")
}

/// Howard Hinnant's `civil_from_days`: days since 1970-01-01 → (year, month, day).
pub(super) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `n` random bytes as hex (`getrandom`, process/time fallback like [`crate::auth`]).
pub(super) fn random_hex(n: usize) -> String {
    let mut bytes = vec![0_u8; n];
    if getrandom::fill(&mut bytes).is_err() {
        let mut state = crate::auth::now_ms()
            .rotate_left(17)
            .wrapping_add(u64::from(std::process::id()))
            | 1;
        for byte in &mut bytes {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            *byte = (state & 0xff) as u8;
        }
    }
    let mut out = String::with_capacity(n * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}
