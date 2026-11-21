use super::{
    cfg, check, io, read, step, write, AgentConfigService, AgentKind, AuthState, BTreeMap, Command,
    Concept, ConfigError, ConfigTarget, Entry, EntryBody, Env, Hook, Layout, McpServer,
    McpTransport, Scope, Skill, Subagent, R, SECRETS,
};

pub(super) fn body_for(concept: Concept, revision: u32) -> Option<EntryBody> {
    let description = Some(format!("Gate {revision}"));
    Some(match concept {
        Concept::Mcp => EntryBody::Mcp(McpServer {
            name: "gate-mcp".into(),
            transport: McpTransport::Stdio,
            command: Some("node".into()),
            args: vec![format!("gate-{revision}.js")],
            env: BTreeMap::from([("GATE_VAR".to_owned(), "plain".to_owned())]),
            ..McpServer::default()
        }),
        Concept::Skill => EntryBody::Skill(Skill {
            name: "gate-skill".into(),
            description,
            body: format!("Do the gate thing {revision}.\n"),
            ..Skill::default()
        }),
        Concept::Command => EntryBody::Command(Command {
            name: "gate-cmd".into(),
            description,
            body: format!("Run gate {revision}.\n"),
            ..Command::default()
        }),
        Concept::Hook => EntryBody::Hook(Hook {
            event: "PreToolUse".into(),
            matcher: Some("Bash".into()),
            hook_type: "command".into(),
            command: format!("echo gate-{revision}"),
            ..Hook::default()
        }),
        Concept::Subagent => EntryBody::Subagent(Subagent {
            name: "gate-agent".into(),
            description,
            body: format!("You are gate agent {revision}.\n"),
            ..Subagent::default()
        }),
        Concept::Instructions => return None,
    })
}

/// The listed entry that corresponds to `body` (hooks: by command), if equal in the fields we wrote.
pub(super) fn find<'a>(entries: &'a [Entry], body: &EntryBody) -> Option<&'a Entry> {
    entries.iter().find(|e| match (&e.body, body) {
        (EntryBody::Mcp(a), EntryBody::Mcp(b)) => {
            a.name == b.name && a.command == b.command && a.args == b.args && a.env == b.env
        }
        (EntryBody::Skill(a), EntryBody::Skill(b)) => {
            a.name == b.name && a.description == b.description && a.body.trim() == b.body.trim()
        }
        (EntryBody::Command(a), EntryBody::Command(b)) => {
            a.name == b.name && a.description == b.description && a.body.trim() == b.body.trim()
        }
        (EntryBody::Hook(a), EntryBody::Hook(b)) => {
            a.event == b.event && a.matcher == b.matcher && a.command == b.command
        }
        (EntryBody::Subagent(a), EntryBody::Subagent(b)) => {
            a.name == b.name && a.description == b.description && a.body.trim() == b.body.trim()
        }
        _ => false,
    })
}

pub(super) fn name_of(entry: &Entry) -> String {
    entry.name.clone()
}

pub(super) async fn concept_roundtrip(svc: &AgentConfigService, layout: &Layout) -> R<()> {
    let agents = [
        AgentKind::ClaudeCode,
        AgentKind::Codex,
        AgentKind::Agy,
        AgentKind::Harness,
    ];
    let mut cells = 0;
    for agent in agents {
        let support: Vec<_> = svc.capabilities(agent);
        for concept in Concept::ALL {
            let row = support
                .iter()
                .find(|s| s.concept == concept)
                .ok_or("missing capability row")?;
            for (scope, supported) in [(Scope::User, row.user), (Scope::Workspace, row.workspace)] {
                let target = ConfigTarget::new(agent, scope);
                let label = format!("{agent:?}/{concept:?}/{scope:?}");
                if !supported {
                    match svc.list(concept, target).await {
                        Err(ConfigError::Unsupported { .. }) => {}
                        other => {
                            return Err(format!(
                                "{label}: expected Unsupported, got {:?}",
                                other.map(|v| v.len())
                            ))
                        }
                    }
                    continue;
                }
                cells += 1;
                if concept == Concept::Instructions {
                    instructions_cell(svc, target, &label).await?;
                } else {
                    entry_cell(svc, concept, target, &label).await?;
                }
            }
        }
    }
    // Unknown keys and comments of the seeded files survived all writes.
    let preserved = [
        (layout.claude_dir.join("settings.json"), "unknownSetting"),
        (layout.claude_dir.join(".claude.json"), "unknownTop"),
        (layout.claude_dir.join(".claude.json"), "customKey"),
        (layout.codex_home.join("config.toml"), "# keep this comment"),
        (layout.codex_home.join("config.toml"), "# inline comment"),
        (
            layout.workspace.join(".claude").join("settings.json"),
            "wsUnknown",
        ),
    ];
    for (file, needle) in preserved {
        check(
            read(&file).contains(needle),
            format!("{} lost {needle}", file.display()),
        )?;
    }
    println!("NOTE concept cells round-tripped: {cells}");
    step("concept_roundtrip");
    Ok(())
}

pub(super) async fn entry_cell(
    svc: &AgentConfigService,
    concept: Concept,
    target: ConfigTarget,
    label: &str,
) -> R<()> {
    let first = body_for(concept, 1).ok_or("no body")?;
    let plan = cfg(label, svc.plan_upsert(target, &first, None).await)?;
    check(!plan.is_no_op(), format!("{label}: create is a no-op"))?;
    cfg(label, svc.apply(&plan).await)?;
    let listed = cfg(label, svc.list(concept, target).await)?;
    let entry = find(&listed, &first).ok_or(format!("{label}: created entry not listed"))?;
    let name = name_of(entry);
    let again = cfg(label, svc.plan_upsert(target, &first, Some(&name)).await)?;
    check(
        again.is_no_op(),
        format!("{label}: same content is not a no-op"),
    )?;
    let second = body_for(concept, 2).ok_or("no body")?;
    let plan = cfg(label, svc.plan_upsert(target, &second, Some(&name)).await)?;
    cfg(label, svc.apply(&plan).await)?;
    let listed = cfg(label, svc.list(concept, target).await)?;
    let entry = find(&listed, &second).ok_or(format!("{label}: update not listed"))?;
    check(
        find(&listed, &first).is_none(),
        format!("{label}: old version still listed"),
    )?;
    let name = name_of(entry);
    let plan = cfg(label, svc.plan_delete(concept, target, &name).await)?;
    cfg(label, svc.apply(&plan).await)?;
    let listed = cfg(label, svc.list(concept, target).await)?;
    check(
        find(&listed, &second).is_none(),
        format!("{label}: entry still listed after delete"),
    )
}

pub(super) async fn instructions_cell(
    svc: &AgentConfigService,
    target: ConfigTarget,
    label: &str,
) -> R<()> {
    let listed = cfg(label, svc.list(Concept::Instructions, target).await)?;
    let file = listed
        .iter()
        .find_map(|e| match &e.body {
            EntryBody::Instructions { file, .. } => Some(file.clone()),
            _ => None,
        })
        .ok_or(format!("{label}: no instruction candidate"))?;
    let before = cfg(label, svc.read_instructions(target, &file.path).await)?;
    let text = format!("{}\n<!-- gate -->\n", before.clone().unwrap_or_default());
    let plan = cfg(
        label,
        svc.plan_instructions(target, &file.path, &text).await,
    )?;
    cfg(label, svc.apply(&plan).await)?;
    let read_back = cfg(label, svc.read_instructions(target, &file.path).await)?;
    check(
        read_back.as_deref() == Some(text.as_str()),
        format!("{label}: read back differs"),
    )?;
    let same = cfg(
        label,
        svc.plan_instructions(target, &file.path, &text).await,
    )?;
    check(
        same.is_no_op(),
        format!("{label}: same text is not a no-op"),
    )?;
    let restore = before.unwrap_or_default();
    let plan = cfg(
        label,
        svc.plan_instructions(target, &file.path, &restore).await,
    )?;
    cfg(label, svc.apply(&plan).await)?;
    Ok(())
}

// ---- 3./4. conflict + masking --------------------------------------------------------------------------

pub(super) async fn conflict(svc: &AgentConfigService, env: &Env) -> R<()> {
    let target = ConfigTarget::new(AgentKind::Harness, Scope::User);
    let body = body_for(Concept::Mcp, 7).ok_or("no body")?;
    let plan = cfg("plan", svc.plan_upsert(target, &body, None).await)?;
    let file = env.home.join("mcp.json");
    let external = "{\n  \"mcpServers\": {\"someone-else\": {\"command\": \"x\"}}\n}\n";
    write(&file, external)?;
    match svc.apply(&plan).await {
        Err(ConfigError::Conflict { .. }) => {}
        other => return Err(format!("expected Conflict, got {other:?}")),
    }
    check(
        read(&file) == external,
        "conflicting apply touched the file",
    )?;
    io("cleanup", std::fs::remove_file(&file))?;
    step("conflict");
    Ok(())
}

pub(super) async fn diff_masking(svc: &AgentConfigService) -> R<()> {
    let codex = ConfigTarget::new(AgentKind::Codex, Scope::User);
    let listed = cfg("codex mcp", svc.list(Concept::Mcp, codex).await)?;
    let mut changes = Vec::new();
    for name in ["local", "remote"] {
        let entry = listed
            .iter()
            .find(|e| e.name == name)
            .ok_or(format!("codex server {name} missing"))?;
        let EntryBody::Mcp(mut server) = entry.body.clone() else {
            return Err("not an MCP entry".into());
        };
        server.args.push("--changed".into());
        server.url = server.url.map(|u| format!("{u}/v2"));
        changes.push(cfg(
            "plan",
            svc.plan_upsert(codex, &EntryBody::Mcp(server), Some(name))
                .await,
        )?);
    }
    let claude = ConfigTarget::new(AgentKind::ClaudeCode, Scope::User);
    let body = body_for(Concept::Mcp, 3).ok_or("no body")?;
    changes.push(cfg("plan", svc.plan_upsert(claude, &body, None).await)?);
    for plan in &changes {
        let mut shown = plan.diff();
        for change in &plan.changes {
            shown.push_str(change.before_preview.as_deref().unwrap_or_default());
            shown.push_str(change.after_preview.as_deref().unwrap_or_default());
            shown.push_str(&format!("{change:?}")[..]);
        }
        check(!shown.trim().is_empty(), "empty preview")?;
        for secret in SECRETS {
            check(
                !shown.contains(secret),
                "a secret appears in a diff or preview",
            )?;
        }
    }
    step("diff_masking");
    Ok(())
}

pub(super) async fn auth(svc: &AgentConfigService) -> R<()> {
    for agent in AgentKind::CLIS {
        let state = svc.auth_state(agent).await;
        check(
            state == AuthState::Present,
            format!("{agent:?} auth state {state:?}, expected Present (fixture file exists)"),
        )?;
    }
    step("auth_presence");
    Ok(())
}
