use super::{
    api, check, io, json, run_turn, step, turns, write, BTreeMap, Chat, Gate, HarnessCore,
    ImportRequest, ImportScan, ImportSource, Path, PathBuf, TurnStatus, A, R,
};

#[allow(clippy::too_many_lines)]
pub(super) fn import_fixtures(root: &Path, workspace: &str) -> R<()> {
    let zcode = root.join("zcode");
    let legacy = json!({
        "meta": {"taskId": "task-1", "title": "Legacy task", "workspacePath": workspace,
                 "createdAt": 1000, "updatedAt": 2000},
        "messages": [
            {"role": "user", "content": "hello legacy"},
            {"role": "assistant", "content": [{"type": "text", "text": "hi"}], "tools": [{"title": "Read file"}]},
            {"role": "user", "content": "second"},
            {"role": "assistant", "content": "done"}
        ]
    });
    write(
        &zcode.join("v2/sessions/ws1/task-1.json"),
        legacy.to_string(),
    )?;
    write(
        &zcode.join("v2/sessions/ws1/task-2.deleted.json"),
        legacy.to_string(),
    )?;
    let db = zcode.join("cli/db/db.sqlite");
    io(
        "mkdir db",
        std::fs::create_dir_all(db.parent().ok_or("db parent")?),
    )?;
    let c = rusqlite::Connection::open(&db).map_err(|e| format!("fixture db: {e}"))?;
    let dir = workspace.replace('\'', "''");
    c.execute_batch(&format!(
        "CREATE TABLE session(id TEXT PRIMARY KEY, project_id TEXT NOT NULL, parent_id TEXT,
            slug TEXT NOT NULL, directory TEXT NOT NULL, title TEXT NOT NULL, version TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, time_archived INTEGER);
         CREATE TABLE message(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, time_created INTEGER NOT NULL,
            time_updated INTEGER NOT NULL, data TEXT NOT NULL, sequence INTEGER);
         CREATE TABLE part(id TEXT PRIMARY KEY, message_id TEXT NOT NULL, session_id TEXT NOT NULL,
            time_created INTEGER NOT NULL, time_updated INTEGER NOT NULL, data TEXT NOT NULL, sequence INTEGER);
         INSERT INTO session VALUES('cli-1','p',NULL,'s','{dir}','CLI session','1',10,30,NULL);
         INSERT INTO session VALUES('cli-archived','p',NULL,'s','{dir}','Old','1',10,20,15);
         INSERT INTO message VALUES('m1','cli-1',11,11,'{{\"role\":\"user\"}}',1);
         INSERT INTO message VALUES('m2','cli-1',12,12,'{{\"role\":\"assistant\"}}',2);
         INSERT INTO part VALUES('p1','m1','cli-1',11,11,'{{\"type\":\"text\",\"text\":\"cli question\"}}',1);
         INSERT INTO part VALUES('p2','m2','cli-1',12,12,'{{\"type\":\"text\",\"text\":\"cli answer\"}}',1);"
    ))
    .map_err(|e| format!("fixture db: {e}"))?;
    drop(c);
    let lines = [
        json!({"type": "user", "cwd": workspace, "timestamp": "2026-01-01T00:00:00Z",
               "message": {"role": "user", "content": "fix the bug<system-reminder>noise</system-reminder>"}}),
        json!({"type": "assistant", "timestamp": "2026-01-01T00:00:05Z", "message": {"content": [
            {"type": "text", "text": "Fixed"}, {"type": "tool_use", "name": "Read", "input": {}}]}}),
    ];
    let jsonl = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    let claude = root.join("claude");
    write(&claude.join("projects/-ws/abc-123.jsonl"), jsonl)?;
    write(&claude.join("settings.json"), "{\"must\":\"not be read\"}")?;
    Ok(())
}

/// Path → (bytes, mtime) of every file under `root`.
pub(super) fn fingerprint(root: &Path) -> R<BTreeMap<PathBuf, (Vec<u8>, std::time::SystemTime)>> {
    let mut out = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in io("read_dir", std::fs::read_dir(&dir))? {
            let path = io("entry", entry)?.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let meta = io("metadata", std::fs::metadata(&path))?;
                let modified = io("mtime", meta.modified())?;
                out.insert(path.clone(), (io("read", std::fs::read(&path))?, modified));
            }
        }
    }
    Ok(out)
}

#[allow(clippy::too_many_lines)]
pub(super) async fn import(chat: &Chat, gate: &Gate) -> R<()> {
    let root = &chat.import_root;
    if root.exists() {
        io("rm import fixtures", std::fs::remove_dir_all(root))?;
    }
    import_fixtures(root, &chat.env.workspace())?;
    let before = fingerprint(root)?;
    let scan = api(
        "import_scan",
        gate.hub
            .import_scan(ImportScan {
                sources: Vec::new(),
            })
            .await,
    )?;
    let mut ids: Vec<&str> = scan.iter().map(|c| c.external_id.as_str()).collect();
    ids.sort_unstable();
    check(
        ids == ["abc-123", "cli-1", "task-1"] && scan.iter().all(|c| !c.imported),
        format!("scan {ids:?}"),
    )?;
    let zcode = api(
        "import zcode",
        gate.hub
            .import_sessions(ImportRequest {
                source: ImportSource::Zcode,
                external_ids: vec!["task-1".into(), "cli-1".into()],
            })
            .await,
    )?;
    let claude = api(
        "import claude",
        gate.hub
            .import_sessions(ImportRequest {
                source: ImportSource::ClaudeCode,
                external_ids: vec!["abc-123".into()],
            })
            .await,
    )?;
    check(
        zcode.imported.len() == 2 && claude.imported.len() == 1,
        format!("imported {zcode:?} {claude:?}"),
    )?;
    let again = api(
        "import again",
        gate.hub
            .import_sessions(ImportRequest {
                source: ImportSource::Zcode,
                external_ids: vec!["task-1".into()],
            })
            .await,
    )?;
    check(
        again.imported.is_empty() && again.skipped.len() == 1,
        "re-import was not deduplicated",
    )?;
    let rescan = api(
        "rescan",
        gate.hub
            .import_scan(ImportScan {
                sources: Vec::new(),
            })
            .await,
    )?;
    check(
        rescan.iter().all(|c| c.imported),
        "rescan does not mark imported",
    )?;
    let legacy = zcode
        .imported
        .iter()
        .find(|s| s.imported_from.as_deref() == Some("zcode:task-1"))
        .ok_or("legacy session missing")?;
    let legacy_turns = turns(gate, &legacy.id).await?;
    check(
        legacy_turns.len() == 2
            && legacy_turns
                .iter()
                .all(|t| t.status == TurnStatus::Completed)
            && legacy_turns[0].tool_calls.len() == 1,
        "imported legacy turns",
    )?;
    let cc = &claude.imported[0];
    let cc_turns = turns(gate, &cc.id).await?;
    check(
        cc_turns.len() == 1 && cc_turns[0].user_text == "fix the bug",
        format!(
            "imported claude turn {:?}",
            cc_turns.first().map(|t| &t.user_text)
        ),
    )?;
    check(
        fingerprint(root)? == before,
        "import changed fixture bytes, mtimes or files",
    )?;
    let news_before = chat
        .env
        .log_entries()
        .iter()
        .filter(|e| e["event"] == "new" && e["agent"] == A)
        .count();
    run_turn(gate, &cc.id, "continue imported").await?;
    let prompts = chat.env.prompts(A, "continue imported");
    let blocks = prompts
        .last()
        .ok_or("imported continue prompt not logged")?;
    check(
        blocks
            .iter()
            .any(|b| b.contains("kind=\"seed\"") && b.contains("fix the bug")),
        "continuing an imported session after a failed session/load did not seed its history",
    )?;
    let created: Vec<String> = chat
        .env
        .log_entries()
        .iter()
        .filter(|e| e["event"] == "new" && e["agent"] == A)
        .skip(news_before)
        .filter_map(|e| e["sessionId"].as_str().map(str::to_owned))
        .collect();
    let native = created
        .last()
        .ok_or("no native session created for the imported session")?;
    let lines = [
        json!({"type": "user", "cwd": chat.env.workspace(), "timestamp": "2026-01-02T00:00:00Z",
               "message": {"role": "user", "content": "harness ran this"}}),
        json!({"type": "assistant", "timestamp": "2026-01-02T00:00:05Z", "message": {"content": "ok"}}),
    ];
    let jsonl = lines
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    write(
        &root.join(format!("claude/projects/-ws/{native}.jsonl")),
        jsonl,
    )?;
    let own = api(
        "scan with a harness native session",
        gate.hub
            .import_scan(ImportScan {
                sources: Vec::new(),
            })
            .await,
    )?;
    check(
        !own.iter().any(|c| &c.external_id == native)
            && own.iter().any(|c| c.external_id == "abc-123" && c.imported),
        "import_scan lists a session Harness itself ran, or lost an imported one",
    )?;
    step("import");
    Ok(())
}
