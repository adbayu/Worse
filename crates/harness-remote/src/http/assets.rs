//! The embedded phone client files (`src/assets/**`), served by `http.rs` without auth.

/// One embedded file served without auth. A fixed table: no path traversal is possible.
pub(super) struct Asset {
    pub(super) name: &'static str,
    pub(super) content_type: &'static str,
    pub(super) body: &'static [u8],
}

/// The phone client (stream C owns `src/assets/**`; this crate only embeds it).
const ASSETS: &[Asset] = &[
    Asset {
        name: "index.html",
        content_type: "text/html; charset=utf-8",
        body: include_bytes!("../assets/index.html"),
    },
    Asset {
        name: "app.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/app.js"),
    },
    Asset {
        name: "style.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/style.css"),
    },
    Asset {
        name: "pair.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/pair.css"),
    },
    Asset {
        name: "sessions.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/sessions.css"),
    },
    Asset {
        name: "chat.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/chat.css"),
    },
    Asset {
        name: "settings.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/settings.css"),
    },
    Asset {
        name: "glass.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/glass.css"),
    },
    Asset {
        name: "refresh.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/refresh.css"),
    },
    Asset {
        name: "avatar.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/avatar.js"),
    },
    Asset {
        name: "newchat.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/newchat.css"),
    },
    Asset {
        name: "newchat.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/newchat.js"),
    },
    Asset {
        name: "theme.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/theme.css"),
    },
    Asset {
        name: "theme.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/theme.js"),
    },
    Asset {
        name: "appearance.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/appearance.js"),
    },
    Asset {
        name: "picker.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/picker.css"),
    },
    Asset {
        name: "picker.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/picker.js"),
    },
    Asset {
        name: "pulls.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/pulls.css"),
    },
    Asset {
        name: "pulls.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/pulls.js"),
    },
    Asset {
        name: "settings_edit.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/settings_edit.css"),
    },
    Asset {
        name: "settings_edit.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/settings_edit.js"),
    },
    Asset {
        name: "usage.css",
        content_type: "text/css; charset=utf-8",
        body: include_bytes!("../assets/usage.css"),
    },
    Asset {
        name: "usage.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/usage.js"),
    },
    Asset {
        name: "usage_charts.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/usage_charts.js"),
    },
    Asset {
        name: "sessions.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/sessions.js"),
    },
    Asset {
        name: "settings.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/settings.js"),
    },
    Asset {
        name: "relay.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/relay.js"),
    },
    Asset {
        name: "manifest.webmanifest",
        content_type: "application/manifest+json",
        body: include_bytes!("../assets/manifest.webmanifest"),
    },
    Asset {
        name: "sw.js",
        content_type: "text/javascript; charset=utf-8",
        body: include_bytes!("../assets/sw.js"),
    },
    Asset {
        name: "icon.svg",
        content_type: "image/svg+xml",
        body: include_bytes!("../assets/icon.svg"),
    },
];

pub(super) fn asset(name: &str) -> Option<&'static Asset> {
    ASSETS.iter().find(|asset| asset.name == name)
}
