use super::{
    present, AppWindow, Cmd, Engine, Instant, Palette, Receiver, Rgba8Pixel, SharedPixelBuffer,
    Shot, Style, FRAME_BUDGET,
};

/// The drawing thread: takes what happened, draws a frame when one is due, hands it to the window.
#[allow(clippy::too_many_lines)]
pub(super) fn work(inbox: &Receiver<Cmd>, window: &slint::Weak<AppWindow>) {
    let mut engine: Option<Engine> = None;
    let mut style = Style::Space;
    let mut palette = Palette::default();
    let mut size = (0_u32, 0_u32, 1.0_f32);
    let mut resting = false;
    let mut inset = 0.0_f32;
    // A frame is on its way to the window; a change came in that no frame shows yet.
    let mut waiting = false;
    let mut dirty = false;
    let mut last = Instant::now();
    let mut fps = 0.0_f32;
    while let Ok(first) = inbox.recv() {
        let mut tick = false;
        for cmd in std::iter::once(first).chain(inbox.try_iter()) {
            match cmd {
                Cmd::Tick => tick = true,
                Cmd::Shown => {
                    waiting = false;
                    tick = true;
                }
                Cmd::Graph(graph) => {
                    engine = graph.map(|graph| {
                        let mut engine = Engine::new(graph, style);
                        engine.set_palette(palette);
                        engine.resize(size.0 as usize, size.1 as usize, size.2);
                        engine.set_paused(resting);
                        engine.set_inset(inset);
                        engine
                    });
                    dirty = true;
                }
                Cmd::Style(next) => {
                    style = next;
                    if let Some(engine) = &mut engine {
                        engine.set_style(next);
                    }
                    dirty = true;
                }
                Cmd::Palette(next) => {
                    if palette != next {
                        palette = next;
                        if let Some(engine) = &mut engine {
                            engine.set_palette(next);
                        }
                        dirty = true;
                    }
                }
                Cmd::Size(width, height, scale) => {
                    if size != (width, height, scale) {
                        size = (width, height, scale);
                        if let Some(engine) = &mut engine {
                            engine.resize(width as usize, height as usize, scale);
                        }
                        dirty = true;
                    }
                }
                Cmd::Rest(rest, calm) => {
                    resting = rest;
                    if let Some(engine) = &mut engine {
                        engine.set_paused(rest);
                        if calm {
                            engine.arrive();
                        }
                    }
                    dirty = true;
                }
                Cmd::Pointer(kind, x, y) => {
                    if let Some(engine) = &mut engine {
                        dirty |= match kind {
                            1 => {
                                engine.pointer_down(x, y);
                                false
                            }
                            2 => {
                                engine.pointer_drag(x, y);
                                true
                            }
                            3 => engine.pointer_up(x, y),
                            4 => engine.pointer_leave(),
                            _ => engine.pointer_move(x, y),
                        };
                    }
                }
                Cmd::Zoom(factor) => {
                    if let Some(engine) = &mut engine {
                        engine.zoom_by(factor);
                    }
                    dirty = true;
                }
                Cmd::Reset => {
                    if let Some(engine) = &mut engine {
                        engine.reset();
                    }
                    dirty = true;
                }
                Cmd::Focus(cluster) => {
                    if let Some(engine) = &mut engine {
                        engine.set_focus(cluster);
                    }
                    dirty = true;
                }
                Cmd::Pick(node) => {
                    if let Some(engine) = &mut engine {
                        engine.select(node, true);
                    }
                    dirty = true;
                }
                Cmd::Inset(width) => {
                    inset = width;
                    if let Some(engine) = &mut engine {
                        engine.set_inset(width);
                    }
                    dirty = true;
                }
            }
        }
        let Some(engine) = engine.as_mut() else {
            continue;
        };
        if waiting || !(dirty || (tick && engine.busy())) {
            continue;
        }
        let (width, height) = engine.size();
        let (Ok(buffer_w), Ok(buffer_h)) = (u32::try_from(width), u32::try_from(height)) else {
            continue;
        };
        if width == 0 || height == 0 {
            continue;
        }
        if let Some(rest) = FRAME_BUDGET.checked_sub(last.elapsed()) {
            std::thread::sleep(rest);
        }
        let now = Instant::now();
        let passed = now.duration_since(last).as_secs_f32();
        last = now;
        engine.step(passed);
        if passed > 0.001 && passed < 0.5 {
            fps = if fps > 1.0 {
                fps * 0.9 + 0.1 / passed
            } else {
                1.0 / passed
            };
        }
        let mut buffer = SharedPixelBuffer::<Rgba8Pixel>::new(buffer_w, buffer_h);
        engine.render(buffer.make_mut_bytes());
        let shot = Shot {
            buffer,
            overlay: engine.overlay(),
            busy: engine.busy(),
            focus: engine.focus(),
            fps,
        };
        dirty = false;
        waiting = true;
        let weak = window.clone();
        let sent = slint::invoke_from_event_loop(move || {
            if let Some(window) = weak.upgrade() {
                present(&window, shot);
            }
        });
        if sent.is_err() {
            return;
        }
    }
}
