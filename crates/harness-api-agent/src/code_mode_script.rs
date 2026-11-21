//! Script text of a `codemode` run: normalization of the model's code and the `JavaScript` sandbox
//! prelude, which defines `console`, `codemode` and nothing else.

/// Console lines kept per run.
const LOGS_MAX: usize = 200;
/// Longest console line kept per run.
const LOG_LINE_MAX: usize = 2000;

/// Strips Markdown fences and wraps a bare statement body in `async () => { ... }`.
pub fn normalize(code: &str) -> String {
    let text = strip_fence(code);
    let body = text.trim();
    if is_function_expression(body) {
        body.to_owned()
    } else {
        format!("async () => {{\n{body}\n}}")
    }
}

fn strip_fence(code: &str) -> String {
    let text = code.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return text.to_owned();
    };
    let rest = rest.split_once('\n').map_or(rest, |(_, after)| after);
    match rest.rsplit_once("```") {
        Some((inner, _)) => inner.to_owned(),
        None => rest.to_owned(),
    }
}

fn is_function_expression(body: &str) -> bool {
    if body.starts_with("function") {
        return true;
    }
    let Some(arrow) = body.find("=>") else {
        return false;
    };
    let head = &body[..arrow];
    head.starts_with("async")
        || (head.starts_with('(') && head.contains(')'))
        || (!head.contains('\n') && !head.contains(';') && !head.contains('{'))
}

/// The prelude builds the pending calls itself and only notifies Rust: a `JavaScript` function that
/// captured a `Ctx` would keep the context alive and the runtime would abort when dropped.
pub fn prelude() -> String {
    format!(
        r#"globalThis.__logs = [];
globalThis.console = {{}};
const __flat = (a) => {{
  try {{ return typeof a === "string" ? a : JSON.stringify(a); }} catch (e) {{ return String(a); }}
}};
const __push = (level, args) => {{
  if (globalThis.__logs.length < {LOGS_MAX}) {{
    globalThis.__logs.push((level + ": " + Array.from(args).map(__flat).join(" ")).slice(0, {LOG_LINE_MAX}));
  }}
}};
console.log = (...a) => __push("log", a);
console.info = (...a) => __push("info", a);
console.warn = (...a) => __push("warn", a);
console.error = (...a) => __push("error", a);
globalThis.codemode = new Proxy({{}}, {{
  get: (_target, name) => {{
    if (typeof name !== "string" || name === "then") return undefined;
    return (args) => globalThis.__call(name, JSON.stringify(args === undefined ? {{}} : args));
  }}
}});
globalThis.__pending = [];
globalThis.__call = (name, args) => new Promise((resolve, reject) => {{
  const marker = globalThis.__notify(name, args);
  if (marker !== "") {{
    reject(marker);
    return;
  }}
  globalThis.__pending.push({{ name: name, args: args, resolve: resolve, reject: reject }});
}});
globalThis.__run = (fn) => (async () => {{
  try {{
    const value = await fn();
    return JSON.stringify({{ result: value === undefined ? null : value, logs: globalThis.__logs }});
  }} catch (e) {{
    const text = (e && e.message) ? (String(e) + "\n" + String(e.stack || "")) : String(e);
    return JSON.stringify({{ error: text, logs: globalThis.__logs }});
  }}
}})();
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_wraps_bare_body() {
        assert_eq!(
            normalize("const a = 1;\nreturn a;"),
            "async () => {\nconst a = 1;\nreturn a;\n}"
        );
    }

    #[test]
    fn normalize_keeps_arrow_function() {
        let code = "async () => {\n  return 1;\n}";
        assert_eq!(normalize(code), code);
        assert_eq!(normalize("async (x) => x + 1"), "async (x) => x + 1");
        assert_eq!(normalize("() => 42"), "() => 42");
        assert_eq!(
            normalize("function () { return 1; }"),
            "function () { return 1; }"
        );
    }

    #[test]
    fn normalize_strips_markdown_fence() {
        let fenced = "```javascript\nasync () => {\n  return 7;\n}\n```";
        assert_eq!(normalize(fenced), "async () => {\n  return 7;\n}");
        let bare = "```\nconst a = 2;\nreturn a;\n```";
        assert_eq!(normalize(bare), "async () => {\nconst a = 2;\nreturn a;\n}");
    }

    #[test]
    fn prelude_defines_only_the_sandbox_globals() {
        let text = prelude();
        assert!(text.contains("globalThis.codemode = new Proxy"));
        assert!(text.contains("console.error"));
        assert!(!text.contains("require("));
        assert!(!text.contains("process."));
    }
}
