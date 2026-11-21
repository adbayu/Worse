use super::{run, sv, with_state, ComponentHandle, Effect, Model, Path};

/// Commands the settings dispatcher does not own (keymap ids and the chat-only actions).
pub fn skill_count() -> usize {
    with_state(|s| s.skills.len()).unwrap_or(0)
}

pub fn command(id: &str) {
    if id == "compactSession" {
        let weak = with_state(|s| s.window.as_weak());
        slint::Timer::single_shot(std::time::Duration::ZERO, move || {
            if let Some(window) = weak.and_then(|w| w.upgrade()) {
                if !window.get_running() {
                    window.invoke_send_draft("/compact".into());
                }
            }
        });
        return;
    }
    let effect = with_state(|s| match id {
        "openCommandCenter" => {
            let ui = s.ui();
            let open = !ui.get_palette_open();
            if open {
                // 修复：上次关闭时停在 Files 等范围，重新打开仍沿用，导致动作搜不到；与 ZCode 一致，每次打开回到 All。
                ui.set_palette_scope("all".into());
                ui.set_palette_index(0);
            }
            ui.set_palette_open(open);
            Effect::None
        }
        "findInTask" => {
            if s.window.get_chat().row_count() > 0 {
                s.ui().set_find_open(true);
            }
            Effect::None
        }
        "toggleSidebar" => {
            s.window.set_drawer_open(!s.window.get_drawer_open());
            Effect::None
        }
        "previousConversation" => s.nav("prev"),
        "nextConversation" => s.nav("next"),
        "navigateBack" => s.nav("back"),
        "navigateForward" => s.nav("forward"),
        "exportMarkdown" => {
            s.export("markdown");
            Effect::None
        }
        "exportHtml" => {
            s.export("html");
            Effect::None
        }
        "copySessionId" => {
            if let Some(id) = s.active.clone() {
                s.copy(&id);
            }
            Effect::None
        }
        "revealWorkspace" => {
            reveal(&s.workspace);
            Effect::None
        }
        "archiveSession" => {
            if let Some(id) = s.active.clone() {
                crate::bridge::archive(id);
            }
            Effect::None
        }
        "renameSession" | "forkSession" | "pinSession" | "scheduledPrompts" | "attachFiles"
        | "whiteboard" => Effect::Later(id.to_string()),
        _ => Effect::None,
    });
    if let Some(effect) = effect {
        run(effect);
    }
}

fn reveal(path: &Path) {
    use harness_platform::PathRevealer;
    // 资源管理器可能阻塞，放到独立线程；失败只显示在错误横幅里。
    let path = path.to_path_buf();
    let weak = with_state(|s| s.window.as_weak());
    std::thread::spawn(move || {
        if let Err(error) = harness_platform::desktop::SystemPathRevealer::new().reveal(&path) {
            let message = error.to_string();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(window) = weak.and_then(|w| w.upgrade()) {
                    window.set_error_text(sv(&message));
                }
            });
        }
    });
}
