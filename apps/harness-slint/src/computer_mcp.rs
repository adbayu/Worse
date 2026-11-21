#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]

use std::fmt::Write as _;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use enigo::{Axis, Button, Coordinate, Direction, Enigo, Key, Keyboard, Mouse, Settings};
use serde_json::{json, Value};

mod actions;
mod helpers;

use helpers::{base64, failure, glide, int, key_of, num, pause, text, tools, typed_preview};

const MAX_SIDE: u32 = 1568;
/// Widest screenshot copy kept for the app's live view (`machine_view.rs`).
const MIRROR_WIDTH: u32 = 1280;
/// The event log starts over when it grows past this.
const EVENTS_MAX: u64 = 512 * 1024;
const OFF: &str =
    "Machine mode is off for this session. Ask the user to switch the mode to Machine.";

#[derive(Clone, Copy)]
struct Frame {
    x: i32,
    y: i32,
    scale: f64,
}

struct Server {
    home: PathBuf,
    session: String,
    frame: Option<Frame>,
    enigo: Option<Enigo>,
    /// Size of the last screenshot in its own pixels (the unit of every tool coordinate).
    shot: (u32, u32),
    seq: u64,
}

/// Directory the app's live view reads: `events.jsonl` (one line per action) and `screen.jpg` (the last
/// screenshot). Kept apart from the mode gate files, which are listed as "sessions with Machine mode on".
pub(crate) fn live_dir(home: &std::path::Path, session: &str) -> PathBuf {
    home.join("machine-live")
        .join(harness_agent_hub::machine::gate_name(session))
}

pub fn run() -> ExitCode {
    let Some(home) = std::env::var_os("HARNESS_HOME").filter(|v| !v.is_empty()) else {
        eprintln!("harness computer-mcp: HARNESS_HOME is not set");
        return ExitCode::FAILURE;
    };
    let mut server = Server {
        home: PathBuf::from(home),
        session: std::env::var("HARNESS_MACHINE_SESSION").unwrap_or_default(),
        frame: None,
        enigo: None,
        shot: (0, 0),
        seq: 0,
    };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(id) = message.get("id").cloned() else {
            continue;
        };
        let method = message
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let params = message.get("params").cloned().unwrap_or(Value::Null);
        let reply = match method {
            "initialize" => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": params.get("protocolVersion").cloned().unwrap_or_else(|| json!("2024-11-05")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "harness-computer", "version": env!("CARGO_PKG_VERSION")}
            }}),
            "ping" => json!({"jsonrpc": "2.0", "id": id, "result": {}}),
            "tools/list" => json!({"jsonrpc": "2.0", "id": id, "result": {"tools": tools()}}),
            "tools/call" => json!({"jsonrpc": "2.0", "id": id, "result": server.call(&params)}),
            other => {
                json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("unknown method {other}")}})
            }
        };
        if writeln!(out, "{reply}").and_then(|()| out.flush()).is_err() {
            break;
        }
    }
    ExitCode::SUCCESS
}

impl Server {
    fn call(&mut self, params: &Value) -> Value {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        if self.session.is_empty()
            || !harness_agent_hub::machine::gate_path(&self.home, &self.session).exists()
        {
            return failure(OFF);
        }
        let result = match name {
            "screenshot" => self.screenshot(&args),
            "click" => self.click(&args),
            "move" => self.point(&args).and_then(|(x, y)| {
                let at = (num(&args, "x")?, num(&args, "y")?);
                self.emit("move", Some(at), &format!("to {}, {}", at.0, at.1));
                glide(self.input()?, x, y)?;
                Ok(text(&format!("Moved to {}, {}", at.0, at.1)))
            }),
            "drag" => self.drag(&args),
            "scroll" => self.scroll(&args),
            "type" => self.type_text(&args),
            "key" => self.keys(&args),
            "windows" => self.windows(),
            "cursor" => self.cursor(),
            other => Err(format!("unknown tool {other}")),
        };
        result.unwrap_or_else(|error| failure(&error))
    }

    /// Tells the app's live view what is about to happen (never fails the tool: the view is optional).
    /// `at` is a point in screenshot pixels.
    fn emit(&mut self, kind: &str, at: Option<(f64, f64)>, text: &str) {
        self.seq += 1;
        let dir = live_dir(&self.home, &self.session);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let path = dir.join("events.jsonl");
        let fresh = std::fs::metadata(&path).is_ok_and(|m| m.len() > EVENTS_MAX);
        let mut event = json!({
            "seq": self.seq,
            "t": std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX)),
            "kind": kind,
            "text": text,
            "w": self.shot.0,
            "h": self.shot.1,
        });
        if let Some((x, y)) = at {
            event["x"] = json!(x);
            event["y"] = json!(y);
        }
        let mut options = std::fs::OpenOptions::new();
        options.create(true);
        if fresh {
            options.write(true).truncate(true);
        } else {
            options.append(true);
        }
        if let Ok(mut file) = options.open(&path) {
            let _ = writeln!(file, "{event}");
        }
    }

    /// Copy of the screenshot for the live view, written whole and then renamed into place.
    fn mirror(&self, image: &image::RgbaImage) {
        let dir = live_dir(&self.home, &self.session);
        if std::fs::create_dir_all(&dir).is_err() {
            return;
        }
        let (w, h) = image.dimensions();
        let rgb = if w > MIRROR_WIDTH {
            let height = ((u64::from(h) * u64::from(MIRROR_WIDTH) / u64::from(w)) as u32).max(1);
            image::DynamicImage::ImageRgba8(image::imageops::thumbnail(image, MIRROR_WIDTH, height))
                .into_rgb8()
        } else {
            image::DynamicImage::ImageRgba8(image.clone()).into_rgb8()
        };
        let tmp = dir.join("screen.jpg.tmp");
        let written = std::fs::File::create(&tmp).is_ok_and(|file| {
            let mut out = std::io::BufWriter::new(file);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 72)
                .encode_image(&rgb)
                .is_ok()
        });
        if written {
            let _ = std::fs::rename(&tmp, dir.join("screen.jpg"));
        }
    }

    fn input(&mut self) -> Result<&mut Enigo, String> {
        if self.enigo.is_none() {
            self.enigo = Some(Enigo::new(&Settings::default()).map_err(|e| e.to_string())?);
        }
        self.enigo
            .as_mut()
            .ok_or_else(|| "input unavailable".to_owned())
    }

    fn frame(&self) -> Result<Frame, String> {
        self.frame
            .ok_or_else(|| "Take a screenshot first so coordinates can be mapped.".to_owned())
    }

    fn map(&self, x: f64, y: f64) -> Result<(i32, i32), String> {
        let f = self.frame()?;
        Ok((
            f.x + (x / f.scale).round() as i32,
            f.y + (y / f.scale).round() as i32,
        ))
    }

    fn point(&self, args: &Value) -> Result<(i32, i32), String> {
        self.map(num(args, "x")?, num(args, "y")?)
    }

    fn screenshot(&mut self, args: &Value) -> Result<Value, String> {
        let monitors = xcap::Monitor::all().map_err(|e| e.to_string())?;
        let chosen = match args.get("monitor").and_then(Value::as_u64) {
            Some(i) => usize::try_from(i).ok().and_then(|i| monitors.get(i)),
            None => monitors
                .iter()
                .find(|m| m.is_primary().unwrap_or(false))
                .or_else(|| monitors.first()),
        }
        .ok_or_else(|| "no such monitor".to_owned())?;
        let image = chosen.capture_image().map_err(|e| e.to_string())?;
        let (w, h) = image.dimensions();
        let long = w.max(h).max(1);
        let scale = if long > MAX_SIDE {
            f64::from(MAX_SIDE) / f64::from(long)
        } else {
            1.0
        };
        let image = if scale < 1.0 {
            image::imageops::resize(
                &image,
                ((f64::from(w) * scale).round() as u32).max(1),
                ((f64::from(h) * scale).round() as u32).max(1),
                image::imageops::FilterType::Triangle,
            )
        } else {
            image
        };
        let mut png = Vec::new();
        image
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .map_err(|e| e.to_string())?;
        self.shot = image.dimensions();
        self.mirror(&image);
        self.emit(
            "screenshot",
            None,
            &format!("{}x{} px", image.width(), image.height()),
        );
        let (mx, my) = (
            chosen.x().map_err(|e| e.to_string())?,
            chosen.y().map_err(|e| e.to_string())?,
        );
        self.frame = Some(Frame {
            x: mx,
            y: my,
            scale,
        });
        let mut info = format!(
            "Screenshot {}x{} px (scale {:.3} of the monitor). Monitors:",
            image.width(),
            image.height(),
            scale
        );
        for (i, m) in monitors.iter().enumerate() {
            let _ = write!(
                info,
                "\n{i}: {} {}x{} at {},{}{}",
                m.name().unwrap_or_default(),
                m.width().unwrap_or_default(),
                m.height().unwrap_or_default(),
                m.x().unwrap_or_default(),
                m.y().unwrap_or_default(),
                if m.is_primary().unwrap_or(false) {
                    " (primary)"
                } else {
                    ""
                }
            );
        }
        Ok(json!({"content": [
            {"type": "image", "data": base64(&png), "mimeType": "image/png"},
            {"type": "text", "text": info}
        ]}))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::helpers::glide_plan;
    use super::*;

    #[test]
    fn glide_is_quick_for_short_hops_and_bounded_for_long_ones() {
        let (steps, each) = glide_plan(10.0);
        assert!((6..=9).contains(&steps), "{steps}");
        assert!(each * u32::try_from(steps).unwrap() <= Duration::from_millis(130));
        let (steps, each) = glide_plan(4000.0);
        assert!(steps <= 24);
        assert!(each * u32::try_from(steps).unwrap() <= Duration::from_millis(361));
    }

    #[test]
    fn typed_preview_is_one_short_line() {
        assert_eq!(typed_preview("ls -la\n"), "ls -la ");
        let long = "x".repeat(200);
        let shown = typed_preview(&long);
        assert_eq!(shown.chars().count(), 61);
        assert!(shown.ends_with('…'));
    }

    #[test]
    fn live_dir_is_apart_from_the_gate_files() {
        let home = std::path::Path::new("/h");
        let dir = live_dir(home, "sess:1/2");
        assert_eq!(dir, std::path::Path::new("/h/machine-live/sess_1_2"));
        assert!(!dir.starts_with(harness_agent_hub::machine::gate_dir(home)));
    }
}
