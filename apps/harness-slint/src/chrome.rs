use super::{ui, workarea, ComponentHandle};

pub(super) fn install_window_chrome(window: &ui::AppWindow) {
    use slint::winit_030::winit::window::ResizeDirection;
    use slint::winit_030::WinitWindowAccessor;
    let weak = window.as_weak();
    let last_down = std::cell::Cell::new(None::<std::time::Instant>);
    let saved = std::rc::Rc::new(std::cell::Cell::new(None::<workarea::Rect>));
    let toggle = {
        let saved = std::rc::Rc::clone(&saved);
        move |window: &ui::AppWindow| {
            let handle = window.window();
            if let Some(rect) = saved.take() {
                workarea::restore(handle, rect);
                window.set_win_maximized(false);
            } else if handle.is_maximized() {
                handle.set_maximized(false);
                window.set_win_maximized(false);
            } else if let Some(rect) = workarea::maximize(handle) {
                saved.set(Some(rect));
                window.set_win_maximized(true);
            } else {
                handle.set_maximized(true);
                window.set_win_maximized(true);
            }
        }
    };
    window.on_window_action(move |action| {
        let Some(window) = weak.upgrade() else {
            return;
        };
        let handle = window.window();
        match action.as_str() {
            "sync" => window.set_win_maximized(saved.get().is_some() || handle.is_maximized()),
            "minimize" => handle.set_minimized(true),
            "maximize" => toggle(&window),
            "close" => handle.dispatch_event(slint::platform::WindowEvent::CloseRequested),
            "drag" => {
                let now = std::time::Instant::now();
                let double = last_down.get().is_some_and(|at| {
                    now.duration_since(at) < std::time::Duration::from_millis(400)
                });
                if double {
                    last_down.set(None);
                    toggle(&window);
                } else {
                    last_down.set(Some(now));
                    // A press on a maximized window only arms the drag: un-maximizing under the
                    // pointer happens on the first real move ("drag-restore"), like the native
                    // title bar. Dragging a maximized window directly does nothing on Windows —
                    // that was the "locked" title bar.
                    if saved.get().is_none() && !handle.is_maximized() {
                        handle.with_winit_window(|w| {
                            let _ = w.drag_window();
                        });
                    }
                }
            }
            rest if rest.starts_with("drag-restore:") => {
                // "drag-restore:<x per-mille of the title bar width>:<pointer y in logical px>"
                let mut fields = rest["drag-restore:".len()..].split(':');
                let fraction = fields
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .map_or(0.5, |per_mille| (per_mille / 1000.0).clamp(0.0, 1.0));
                let top = fields
                    .next()
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);
                #[allow(clippy::cast_possible_truncation)]
                let top = (top * f64::from(handle.scale_factor())).round() as i32;
                let saved_rect = saved.take();
                if !workarea::place_dragged(handle, saved_rect, fraction, top) {
                    // No Win32 placement (or no cursor): fall back to the saved rect.
                    if let Some(rect) = saved_rect {
                        workarea::restore(handle, rect);
                    }
                }
                // winit caches the maximized flag: drop it so the restored geometry sticks.
                if handle.is_maximized() {
                    handle.set_maximized(false);
                }
                window.set_win_maximized(false);
                handle.with_winit_window(|w| {
                    let _ = w.drag_window();
                });
            }
            other => {
                let direction = match other.strip_prefix("resize-") {
                    Some("n") => ResizeDirection::North,
                    Some("s") => ResizeDirection::South,
                    Some("e") => ResizeDirection::East,
                    Some("w") => ResizeDirection::West,
                    Some("ne") => ResizeDirection::NorthEast,
                    Some("nw") => ResizeDirection::NorthWest,
                    Some("se") => ResizeDirection::SouthEast,
                    Some("sw") => ResizeDirection::SouthWest,
                    _ => return,
                };
                handle.with_winit_window(|w| {
                    let _ = w.drag_resize_window(direction);
                });
            }
        }
    });
}
