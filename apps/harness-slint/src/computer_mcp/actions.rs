use super::{
    glide, int, key_of, num, pause, text, typed_preview, Axis, Button, Coordinate, Direction,
    Duration, Keyboard, Mouse, Server, Value,
};

impl Server {
    pub(super) fn click(&mut self, args: &Value) -> Result<Value, String> {
        let (x, y) = self.point(args)?;
        let button = match args.get("button").and_then(Value::as_str).unwrap_or("left") {
            "right" => Button::Right,
            "middle" => Button::Middle,
            _ => Button::Left,
        };
        let count = int(args, "count", 1).clamp(1, 3);
        let at = (num(args, "x")?, num(args, "y")?);
        let (kind, name) = match (button, count) {
            (Button::Right, _) => ("right", "right"),
            (Button::Middle, _) => ("click", "middle"),
            (_, 1) => ("click", "left"),
            _ => ("double", "left"),
        };
        self.emit(kind, Some(at), &format!("{name} at {}, {}", at.0, at.1));
        let input = self.input()?;
        glide(input, x, y)?;
        pause();
        for _ in 0..count {
            input
                .button(button, Direction::Click)
                .map_err(|e| e.to_string())?;
        }
        Ok(text(&format!(
            "Clicked {} time(s) at {}, {}",
            count,
            num(args, "x")?,
            num(args, "y")?
        )))
    }

    pub(super) fn drag(&mut self, args: &Value) -> Result<Value, String> {
        let (x, y) = self.point(args)?;
        let (tx, ty) = self.map(num(args, "to_x")?, num(args, "to_y")?)?;
        let from = (num(args, "x")?, num(args, "y")?);
        let to = (num(args, "to_x")?, num(args, "to_y")?);
        self.emit(
            "drag",
            Some(from),
            &format!("from {}, {} to {}, {}", from.0, from.1, to.0, to.1),
        );
        glide(self.input()?, x, y)?;
        pause();
        self.emit("drop", Some(to), &format!("at {}, {}", to.0, to.1));
        let input = self.input()?;
        input
            .button(Button::Left, Direction::Press)
            .map_err(|e| e.to_string())?;
        for step in 1..=12 {
            let px = x + (tx - x) * step / 12;
            let py = y + (ty - y) * step / 12;
            input
                .move_mouse(px, py, Coordinate::Abs)
                .map_err(|e| e.to_string())?;
            std::thread::sleep(Duration::from_millis(25));
        }
        input
            .button(Button::Left, Direction::Release)
            .map_err(|e| e.to_string())?;
        Ok(text("Dragged"))
    }

    pub(super) fn scroll(&mut self, args: &Value) -> Result<Value, String> {
        let (x, y) = self.point(args)?;
        let dx = i32::try_from(int(args, "dx", 0)).unwrap_or(0);
        let dy = i32::try_from(int(args, "dy", 0)).unwrap_or(0);
        let at = (num(args, "x")?, num(args, "y")?);
        let way = match (dy, dx) {
            (d, _) if d > 0 => format!("down {d}"),
            (d, _) if d < 0 => format!("up {}", -d),
            (_, d) if d > 0 => format!("right {d}"),
            (_, d) => format!("left {}", -d),
        };
        self.emit("scroll", Some(at), &format!("{way} at {}, {}", at.0, at.1));
        let input = self.input()?;
        glide(input, x, y)?;
        pause();
        if dy != 0 {
            input
                .scroll(dy, Axis::Vertical)
                .map_err(|e| e.to_string())?;
        }
        if dx != 0 {
            input
                .scroll(dx, Axis::Horizontal)
                .map_err(|e| e.to_string())?;
        }
        Ok(text("Scrolled"))
    }

    pub(super) fn type_text(&mut self, args: &Value) -> Result<Value, String> {
        let value = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing text".to_owned())?
            .to_owned();
        self.emit("type", None, &format!("“{}”", typed_preview(&value)));
        self.input()?.text(&value).map_err(|e| e.to_string())?;
        Ok(text(&format!("Typed {} characters", value.chars().count())))
    }

    pub(super) fn keys(&mut self, args: &Value) -> Result<Value, String> {
        let keys = args
            .get("keys")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing keys".to_owned())?
            .to_owned();
        let combos = keys
            .split_whitespace()
            .map(|combo| combo.split('+').map(key_of).collect::<Result<Vec<_>, _>>())
            .collect::<Result<Vec<_>, _>>()?;
        self.emit("key", None, &keys);
        let input = self.input()?;
        for combo in combos {
            let Some((last, mods)) = combo.split_last() else {
                continue;
            };
            for m in mods {
                input.key(*m, Direction::Press).map_err(|e| e.to_string())?;
            }
            let result = input
                .key(*last, Direction::Click)
                .map_err(|e| e.to_string());
            for m in mods.iter().rev() {
                let _ = input.key(*m, Direction::Release);
            }
            result?;
            pause();
        }
        Ok(text(&format!("Pressed {keys}")))
    }

    pub(super) fn windows(&self) -> Result<Value, String> {
        let windows = xcap::Window::all().map_err(|e| e.to_string())?;
        let mut lines = Vec::new();
        for w in windows {
            if w.is_minimized().unwrap_or(false) {
                continue;
            }
            let title = w.title().unwrap_or_default();
            if title.trim().is_empty() {
                continue;
            }
            let (x, y) = (w.x().unwrap_or_default(), w.y().unwrap_or_default());
            let (width, height) = (
                w.width().unwrap_or_default(),
                w.height().unwrap_or_default(),
            );
            let shot = self.frame.map_or_else(String::new, |f| {
                format!(
                    " | screenshot {},{} {}x{}",
                    (f64::from(x - f.x) * f.scale).round(),
                    (f64::from(y - f.y) * f.scale).round(),
                    (f64::from(width) * f.scale).round(),
                    (f64::from(height) * f.scale).round()
                )
            });
            lines.push(format!(
                "{title} [{}]{} screen {x},{y} {width}x{height}{shot}",
                w.app_name().unwrap_or_default(),
                if w.is_focused().unwrap_or(false) {
                    " (focused)"
                } else {
                    ""
                }
            ));
        }
        Ok(text(&lines.join("\n")))
    }

    pub(super) fn cursor(&mut self) -> Result<Value, String> {
        let frame = self.frame;
        let (x, y) = self.input()?.location().map_err(|e| e.to_string())?;
        Ok(match frame {
            Some(f) => text(&format!(
                "{}, {}",
                (f64::from(x - f.x) * f.scale).round(),
                (f64::from(y - f.y) * f.scale).round()
            )),
            None => text(&format!(
                "screen {x}, {y} (take a screenshot to map coordinates)"
            )),
        })
    }
}
