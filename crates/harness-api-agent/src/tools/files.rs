use super::*;

impl Ctx<'_> {
    pub(super) fn read_image(&self, path: &Path, mime: &str, bytes: &[u8]) -> Outcome {
        use base64::Engine as _;
        if !self.agent.vision() {
            return Outcome::err("image file: this model cannot view images");
        }
        if bytes.len() > IMAGE_BYTES_MAX {
            return Outcome::err("image is larger than 5 MB");
        }
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        let Ok(mut images) = self.images.lock() else {
            return Outcome::err("image could not be attached");
        };
        images.push(crate::llm::image_part(mime, &data));
        Outcome::ok(format!(
            "Image {} ({} bytes) is attached in the next message.",
            path.display(),
            bytes.len()
        ))
    }

    pub(super) async fn read_file(&self, args: &Value) -> Outcome {
        let Some(path) = str_arg(args, "path") else {
            return Outcome::err("path is required");
        };
        let path = resolve(self.cwd, path);
        match tokio::fs::metadata(&path).await {
            Ok(meta) if meta.is_dir() => return Outcome::err("path is a directory; use list_dir"),
            Ok(meta) if meta.len() > READ_BYTES_MAX => {
                return Outcome::err("file is larger than 10 MB")
            }
            Ok(_) => {}
            Err(e) => return Outcome::err(format!("{}: {e}", path.display())),
        }
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(e) => return Outcome::err(format!("{}: {e}", path.display())),
        };
        if let Some(mime) = image_mime(&path) {
            return self.read_image(&path, mime, &bytes);
        }
        if bytes.iter().take(8192).any(|b| *b == 0) {
            return Outcome::err("binary file");
        }
        let text = String::from_utf8_lossy(&bytes);
        let offset = args
            .get("offset")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(1)
            .max(1);
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .and_then(|v| usize::try_from(v).ok())
            .unwrap_or(READ_LINES)
            .clamp(1, READ_LINES);
        let total = text.lines().count();
        let mut out = String::new();
        for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
            let line: String = line.chars().take(LINE_MAX).collect();
            let _ = writeln!(out, "{:>6}\t{line}", i + 1);
        }
        let last = (offset - 1 + limit).min(total);
        if last < total {
            let _ = writeln!(
                out,
                "… {} more lines (file has {total}); use offset={}",
                total - last,
                last + 1
            );
        }
        if out.is_empty() {
            out = "(empty file)".into();
        }
        let mut outcome = Outcome::ok(out);
        outcome.content = vec![
            json!({"type": "content", "content": {"type": "text", "text": format!("{} lines", last.saturating_sub(offset - 1))}}),
        ];
        outcome
    }

    pub(super) async fn list_dir(&self, args: &Value) -> Outcome {
        let path = resolve(self.cwd, str_arg(args, "path").unwrap_or("."));
        let mut entries = match tokio::fs::read_dir(&path).await {
            Ok(entries) => entries,
            Err(e) => return Outcome::err(format!("{}: {e}", path.display())),
        };
        let mut names = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let mut name = entry.file_name().to_string_lossy().into_owned();
            if entry.file_type().await.is_ok_and(|t| t.is_dir()) {
                name.push('/');
            }
            names.push(name);
        }
        names.sort();
        let more = names.len().saturating_sub(LIST_MAX);
        names.truncate(LIST_MAX);
        let mut out = names.join("\n");
        if more > 0 {
            let _ = write!(out, "\n… {more} more entries");
        }
        if out.is_empty() {
            out = "(empty directory)".into();
        }
        Outcome::ok(out)
    }

    pub(super) async fn search(&self, args: &Value) -> Outcome {
        let Some(pattern) = str_arg(args, "pattern").map(str::to_owned) else {
            return Outcome::err("pattern is required");
        };
        let flag = |key: &str| args.get(key).and_then(Value::as_bool).unwrap_or(false);
        let request = crate::search::Request {
            root: resolve(self.cwd, str_arg(args, "path").unwrap_or(".")),
            cwd: self.cwd.to_path_buf(),
            pattern,
            glob: str_arg(args, "glob").map(str::to_owned),
            insensitive: flag("case_insensitive"),
            files_only: flag("files_only"),
            context: args.get("context").and_then(Value::as_u64).map_or(0, |n| {
                usize::try_from(n).unwrap_or(crate::search::CONTEXT_MAX)
            }),
        };
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = std::sync::Arc::clone(&cancel);
        let task = tokio::task::spawn_blocking(move || {
            crate::search::run(&request, &flag, crate::search::DEADLINE)
        });
        let result = tokio::select! {
            () = self.token.cancelled() => {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                return Outcome::err("cancelled");
            }
            result = task => result,
        };
        match result {
            Ok(Ok(text)) => Outcome::ok(text),
            Ok(Err(e)) => Outcome::err(e),
            Err(e) => Outcome::err(e.to_string()),
        }
    }

    pub(super) async fn write_file(&self, args: &Value) -> Outcome {
        let (Some(path), Some(content)) = (str_arg(args, "path"), str_arg(args, "content")) else {
            return Outcome::err("path and content are required");
        };
        let path = resolve(self.cwd, path);
        let old = tokio::fs::read_to_string(&path).await.ok();
        if let Some(parent) = path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                return Outcome::err(format!("{}: {e}", parent.display()));
            }
        }
        if let Err(e) = tokio::fs::write(&path, content).await {
            return Outcome::err(format!("{}: {e}", path.display()));
        }
        let mut outcome = Outcome::ok(format!(
            "wrote {} ({} bytes)",
            path.display(),
            content.len()
        ));
        outcome.content = vec![
            json!({"type": "diff", "path": path.to_string_lossy(), "oldText": old, "newText": content}),
        ];
        outcome
    }

    pub(super) async fn edit_file(&self, args: &Value) -> Outcome {
        let (Some(path), Some(old_string), Some(new_string)) = (
            str_arg(args, "path"),
            str_arg(args, "old_string"),
            str_arg(args, "new_string"),
        ) else {
            return Outcome::err("path, old_string and new_string are required");
        };
        if old_string.is_empty() {
            return Outcome::err("old_string must not be empty; use write_file to create a file");
        }
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let path = resolve(self.cwd, path);
        let old = match tokio::fs::read_to_string(&path).await {
            Ok(text) => text,
            Err(e) => return Outcome::err(format!("{}: {e}", path.display())),
        };
        let count = old.matches(old_string).count();
        if count == 0 {
            return Outcome::err("old_string was not found in the file");
        }
        if count > 1 && !replace_all {
            return Outcome::err(format!(
                "old_string was found {count} times; add more context or set replace_all"
            ));
        }
        let new = if replace_all {
            old.replace(old_string, new_string)
        } else {
            old.replacen(old_string, new_string, 1)
        };
        if let Err(e) = tokio::fs::write(&path, &new).await {
            return Outcome::err(format!("{}: {e}", path.display()));
        }
        let mut outcome = Outcome::ok(format!(
            "edited {} ({count} replacement(s))",
            path.display()
        ));
        outcome.content = vec![
            json!({"type": "diff", "path": path.to_string_lossy(), "oldText": old, "newText": new}),
        ];
        outcome
    }
}
