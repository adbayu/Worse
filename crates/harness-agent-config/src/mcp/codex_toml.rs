use super::{
    fsx, validate, BTreeMap, ConfigError, DocumentMut, FileChange, Item, Kind, McpServer,
    McpTransport, Path, PathBuf, Result, Table, Value,
};

// ---------------------------------------------------------------------------------------------------------------
// Codex `config.toml` (`[mcp_servers.<name>]`), edited with toml_edit so comments and unknown keys survive.

pub(super) const CODEX_KNOWN: &[&str] = &[
    "command",
    "args",
    "env",
    "url",
    "http_headers",
    "enabled",
    "startup_timeout_sec",
    "tool_timeout_sec",
];

pub(crate) struct TomlFile {
    path: PathBuf,
    before: Option<String>,
    pub doc: DocumentMut,
}

impl TomlFile {
    pub(crate) async fn load(path: &Path) -> Result<Self> {
        let before = fsx::read_to_string_opt(path).await?;
        let doc = before
            .as_deref()
            .unwrap_or("")
            .parse::<DocumentMut>()
            .map_err(|e| ConfigError::invalid(format!("{}: {e}", path.display())))?;
        Ok(Self {
            path: path.to_path_buf(),
            before,
            doc,
        })
    }

    pub(super) fn servers(&self) -> Option<&Table> {
        self.doc.get("mcp_servers").and_then(Item::as_table)
    }

    pub(super) fn servers_mut(&mut self) -> &mut Table {
        let item = self.doc.entry("mcp_servers").or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(true);
            Item::Table(t)
        });
        if !item.is_table() {
            let mut t = Table::new();
            t.set_implicit(true);
            *item = Item::Table(t);
        }
        match item {
            Item::Table(t) => t,
            _ => unreachable!("item was just set to a table"),
        }
    }

    pub(crate) fn into_change(self) -> FileChange {
        let text = self.doc.to_string();
        FileChange::write(self.path, self.before, text, Kind::Toml)
    }
}

pub(crate) fn toml_to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::String(s) => serde_json::Value::String(s.value().clone()),
        Value::Integer(i) => serde_json::Value::from(*i.value()),
        Value::Float(f) => serde_json::Number::from_f64(*f.value())
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Boolean(b) => serde_json::Value::Bool(*b.value()),
        Value::Datetime(d) => serde_json::Value::String(d.value().to_string()),
        Value::Array(a) => serde_json::Value::Array(a.iter().map(toml_to_json).collect()),
        Value::InlineTable(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), toml_to_json(v)))
                .collect(),
        ),
    }
}

pub(super) fn item_to_json(item: &Item) -> serde_json::Value {
    match item {
        Item::Value(v) => toml_to_json(v),
        Item::Table(t) => serde_json::Value::Object(
            t.iter()
                .map(|(k, v)| (k.to_owned(), item_to_json(v)))
                .collect(),
        ),
        Item::ArrayOfTables(a) => serde_json::Value::Array(
            a.iter()
                .map(|t| {
                    serde_json::Value::Object(
                        t.iter()
                            .map(|(k, v)| (k.to_owned(), item_to_json(v)))
                            .collect(),
                    )
                })
                .collect(),
        ),
        Item::None => serde_json::Value::Null,
    }
}

pub(super) fn json_to_toml(value: &serde_json::Value) -> Option<Value> {
    Some(match value {
        serde_json::Value::Null => return None,
        serde_json::Value::Bool(b) => Value::from(*b),
        serde_json::Value::Number(n) => match n.as_i64() {
            Some(i) => Value::from(i),
            None => Value::from(n.as_f64()?),
        },
        serde_json::Value::String(s) => Value::from(s.as_str()),
        serde_json::Value::Array(items) => {
            Value::Array(items.iter().filter_map(json_to_toml).collect())
        }
        serde_json::Value::Object(map) => {
            let mut t = toml_edit::InlineTable::new();
            for (k, v) in map {
                if let Some(v) = json_to_toml(v) {
                    t.insert(k, v);
                }
            }
            Value::InlineTable(t)
        }
    })
}

/// Sets `key` to `desired` (JSON form) unless the current item already means the same (keeps its formatting).
pub(super) fn set_toml(table: &mut Table, key: &str, desired: Option<serde_json::Value>) {
    let current = table.get(key).map(item_to_json);
    match desired {
        None | Some(serde_json::Value::Null) => {
            table.remove(key);
        }
        Some(d) if current.as_ref() == Some(&d) => {}
        Some(d) => {
            if let (Some(Item::Table(sub)), serde_json::Value::Object(map)) =
                (table.get_mut(key), &d)
            {
                // A `[x.env]` sub-table stays a sub-table: rewrite its keys in place.
                sub.clear();
                for (k, v) in map {
                    if let Some(v) = json_to_toml(v) {
                        sub.insert(k, Item::Value(v));
                    }
                }
                return;
            }
            if let Some(v) = json_to_toml(&d) {
                table.insert(key, Item::Value(v));
            }
        }
    }
}

pub(super) fn str_map_json(map: &BTreeMap<String, String>) -> Option<serde_json::Value> {
    (!map.is_empty()).then(|| {
        serde_json::Value::Object(
            map.iter()
                .map(|(k, v)| (k.clone(), v.clone().into()))
                .collect(),
        )
    })
}

pub(super) fn f64_json(v: Option<f64>) -> Option<serde_json::Value> {
    v.and_then(crate::jsonfile::number)
        .map(serde_json::Value::Number)
}

pub(super) fn read_codex(name: &str, table: &Table) -> McpServer {
    let json = item_to_json(&Item::Table(table.clone()));
    let get = |k: &str| json.get(k);
    let str_of = |k: &str| {
        get(k)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let map_of = |k: &str| -> BTreeMap<String, String> {
        get(k)
            .and_then(serde_json::Value::as_object)
            .map(|m| {
                m.iter()
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default()
    };
    let url = str_of("url");
    McpServer {
        name: name.to_owned(),
        transport: if url.is_some() {
            McpTransport::Http
        } else {
            McpTransport::Stdio
        },
        command: str_of("command"),
        args: get("args")
            .and_then(serde_json::Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        env: map_of("env"),
        url,
        headers: map_of("http_headers"),
        enabled: Some(
            get("enabled")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true),
        ),
        startup_timeout_sec: get("startup_timeout_sec").and_then(serde_json::Value::as_f64),
        tool_timeout_sec: get("tool_timeout_sec").and_then(serde_json::Value::as_f64),
        agents: Vec::new(),
        extra: json
            .as_object()
            .map(|m| {
                m.iter()
                    .filter(|(k, _)| !CODEX_KNOWN.contains(&k.as_str()))
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect()
            })
            .unwrap_or_default(),
    }
}

pub(crate) async fn list_toml(path: &Path) -> Result<Vec<McpServer>> {
    let file = TomlFile::load(path).await?;
    Ok(file
        .servers()
        .map(|servers| {
            servers
                .iter()
                .filter_map(|(name, item)| item.as_table().map(|t| read_codex(name, t)))
                .collect()
        })
        .unwrap_or_default())
}

pub(crate) async fn plan_upsert_toml(
    path: &Path,
    server: &McpServer,
    previous_name: Option<&str>,
) -> Result<FileChange> {
    validate(server)?;
    if server.transport == McpTransport::Sse {
        return Err(ConfigError::invalid(
            "Codex supports stdio and streamable http servers, not sse",
        ));
    }
    let mut file = TomlFile::load(path).await?;
    let servers = file.servers_mut();
    if let Some(previous) = previous_name.filter(|p| *p != server.name) {
        if servers.contains_key(&server.name) {
            return Err(ConfigError::invalid(format!(
                "a server named {} already exists",
                server.name
            )));
        }
        let item = servers
            .remove(previous)
            .ok_or_else(|| ConfigError::NotFound(previous.to_owned()))?;
        servers.insert(&server.name, item);
    }
    if !servers.get(&server.name).is_some_and(Item::is_table) {
        servers.insert(&server.name, Item::Table(Table::new()));
    }
    let Some(Item::Table(table)) = servers.get_mut(&server.name) else {
        unreachable!("server table was just inserted");
    };
    let stdio = server.transport == McpTransport::Stdio;
    set_toml(
        table,
        "command",
        server.command.clone().filter(|_| stdio).map(Into::into),
    );
    set_toml(
        table,
        "args",
        (stdio && (!server.args.is_empty() || table.contains_key("args"))).then(|| {
            serde_json::Value::Array(server.args.iter().cloned().map(Into::into).collect())
        }),
    );
    set_toml(table, "env", str_map_json(&server.env));
    set_toml(
        table,
        "url",
        server.url.clone().filter(|_| !stdio).map(Into::into),
    );
    set_toml(
        table,
        "http_headers",
        if stdio {
            None
        } else {
            str_map_json(&server.headers)
        },
    );
    let enabled = server.enabled.unwrap_or(true);
    set_toml(
        table,
        "enabled",
        (!enabled || table.contains_key("enabled")).then_some(serde_json::Value::Bool(enabled)),
    );
    set_toml(
        table,
        "startup_timeout_sec",
        f64_json(server.startup_timeout_sec),
    );
    set_toml(table, "tool_timeout_sec", f64_json(server.tool_timeout_sec));
    let stale: Vec<String> = table
        .iter()
        .map(|(k, _)| k.to_owned())
        .filter(|k| !CODEX_KNOWN.contains(&k.as_str()) && !server.extra.contains_key(k))
        .collect();
    for k in stale {
        table.remove(&k);
    }
    for (k, v) in &server.extra {
        if !CODEX_KNOWN.contains(&k.as_str()) {
            set_toml(table, k, Some(v.clone()));
        }
    }
    Ok(file.into_change())
}

pub(crate) async fn plan_delete_toml(path: &Path, name: &str) -> Result<FileChange> {
    let mut file = TomlFile::load(path).await?;
    if file.servers_mut().remove(name).is_none() {
        return Err(ConfigError::NotFound(name.to_owned()));
    }
    Ok(file.into_change())
}

pub(crate) async fn plan_enabled_toml(
    path: &Path,
    name: &str,
    enabled: bool,
) -> Result<FileChange> {
    let mut server = list_toml(path)
        .await?
        .into_iter()
        .find(|s| s.name == name)
        .ok_or_else(|| ConfigError::NotFound(name.to_owned()))?;
    server.enabled = Some(enabled);
    plan_upsert_toml(path, &server, None).await
}
