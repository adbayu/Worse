use super::{
    ends_without_newline, fs, new_id, normalized, now_ms, project_key, redact, secret_reason,
    Added, Kind, LockGuard, Memory, NewMemory, OpenOptions, Record, Store, Write, COMPACT_MIN_DEAD,
    EPISODE_TTL_MS, LOCK, MAX_TEXT_CHARS,
};

impl Store {
    /// Live memories and the number of dead records in the file.
    pub(super) fn fold(&self, now: i64) -> (Vec<Memory>, usize) {
        let Ok(text) = fs::read_to_string(self.log_path()) else {
            return (Vec::new(), 0);
        };
        let mut live: Vec<Memory> = Vec::new();
        let mut lines = 0usize;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            lines += 1;
            let Ok(record) = serde_json::from_str::<Record>(line) else {
                continue;
            };
            match record {
                Record::Put(memory) => {
                    live.retain(|m| {
                        m.id != memory.id
                            && memory.replaces.as_deref() != Some(m.id.as_str())
                            && !(memory.key.is_some() && m.key == memory.key)
                    });
                    live.push(memory);
                }
                Record::Forget { id, .. } => live.retain(|m| m.id != id),
                Record::Pin { id, pinned, .. } => {
                    if let Some(memory) = live.iter_mut().find(|m| m.id == id) {
                        memory.pinned = pinned;
                    }
                }
            }
        }
        live.retain(|m| m.expires.map_or(true, |at| at > now));
        let dead = lines.saturating_sub(live.len());
        (live, dead)
    }

    pub(super) fn append(&self, records: &[Record]) -> std::io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let _guard = LockGuard::acquire(&self.dir.join(LOCK))?;
        let mut out = String::new();
        // A crash can leave the file without a final newline: start on a fresh line so the torn record does
        // not swallow this one.
        let needs_newline = ends_without_newline(&self.log_path());
        if needs_newline {
            out.push('\n');
        }
        for record in records {
            out.push_str(&serde_json::to_string(record).map_err(std::io::Error::other)?);
            out.push('\n');
        }
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())?;
        file.write_all(out.as_bytes())?;
        file.sync_data()?;
        drop(file);
        self.compact_locked();
        Ok(())
    }

    /// Rewrites the log with the live records only (caller holds the lock). Failure leaves the old file.
    pub(super) fn compact_locked(&self) {
        let (live, dead) = self.fold(now_ms());
        if dead < COMPACT_MIN_DEAD || dead <= live.len() {
            return;
        }
        let mut out = String::new();
        for memory in live {
            if let Ok(line) = serde_json::to_string(&Record::Put(memory)) {
                out.push_str(&line);
                out.push('\n');
            }
        }
        let tmp = self.dir.join(format!(".recall.{}.tmp", std::process::id()));
        let written = fs::File::create(&tmp).and_then(|mut file| {
            file.write_all(out.as_bytes())?;
            file.sync_all()
        });
        if written.is_ok() {
            let _ = fs::rename(&tmp, self.log_path());
        } else {
            let _ = fs::remove_file(&tmp);
        }
    }

    /// Saves a memory. The same text already stored in the same scope is not stored twice.
    ///
    /// # Errors
    ///
    /// A message for the caller: empty or too long text, text that looks like a credential, or an I/O error.
    pub fn add(&self, new: NewMemory) -> Result<Added, String> {
        let text = new.text.trim().to_owned();
        if text.is_empty() {
            return Err("text is empty".into());
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(format!(
                "text is longer than {MAX_TEXT_CHARS} characters: save one fact per memory, in a sentence or two"
            ));
        }
        if let Some(reason) = secret_reason(&text) {
            return Err(format!(
                "not saved: the text {reason}. Never store credentials in memory"
            ));
        }
        let kind = new.kind.unwrap_or(Kind::Fact);
        if kind == Kind::Episode {
            return Err("episodes are written by Harness, not added".into());
        }
        let live = self.load();
        let same = normalized(&text);
        if let Some(existing) = live.iter().find(|m| {
            m.kind != Kind::Episode
                && m.project == new.project
                && normalized(&m.text) == same
                && new.replaces.as_deref() != Some(m.id.as_str())
        }) {
            return Ok(Added::Duplicate(existing.id.clone()));
        }
        if let Some(old) = &new.replaces {
            if !live.iter().any(|m| &m.id == old && m.kind != Kind::Episode) {
                return Err(format!("no memory with id {old} to replace"));
            }
        }
        let now = now_ms();
        let memory = Memory {
            id: new_id(),
            at: now,
            kind,
            text,
            project: new.project,
            pinned: new.pinned.unwrap_or(kind == Kind::Preference),
            session: new.session,
            agent: new.agent,
            title: None,
            key: None,
            replaces: new.replaces,
            expires: new
                .ttl_days
                .map(|days| now + i64::from(days) * 24 * 60 * 60 * 1000),
        };
        let id = memory.id.clone();
        self.append(&[Record::Put(memory)])
            .map_err(|e| format!("write failed: {e}"))?;
        Ok(Added::New(id))
    }

    /// Writes the episode of a session, taking the place of its earlier one. Credentials in `text` are
    /// redacted, not refused: the digest is cut from the conversation, nobody chose its words.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn put_episode(
        &self,
        session: &str,
        project: &str,
        title: &str,
        agent: &str,
        text: &str,
    ) -> std::io::Result<()> {
        let now = now_ms();
        let text: String = redact(text).chars().take(MAX_TEXT_CHARS * 2).collect();
        let memory = Memory {
            id: new_id(),
            at: now,
            kind: Kind::Episode,
            text,
            project: Some(project_key(project)),
            pinned: false,
            session: Some(session.to_owned()),
            agent: Some(agent.to_owned()).filter(|a| !a.is_empty()),
            title: Some(redact(title)).filter(|t| !t.trim().is_empty()),
            key: Some(format!("session:{session}")),
            replaces: None,
            expires: Some(now + EPISODE_TTL_MS),
        };
        self.append(&[Record::Put(memory)])
    }

    /// Forgets the memory with this id, or, when no id matches, the memories whose whole text is `target`.
    /// Returns how many were forgotten.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn forget(&self, target: &str) -> std::io::Result<usize> {
        let target = target.trim();
        let live = self.load();
        let mut ids: Vec<String> = live
            .iter()
            .filter(|m| m.id == target)
            .map(|m| m.id.clone())
            .collect();
        if ids.is_empty() && !target.is_empty() {
            let same = normalized(target);
            ids = live
                .iter()
                .filter(|m| normalized(&m.text) == same)
                .map(|m| m.id.clone())
                .collect();
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let at = now_ms();
        let records: Vec<Record> = ids
            .iter()
            .map(|id| Record::Forget { id: id.clone(), at })
            .collect();
        self.append(&records)?;
        Ok(ids.len())
    }

    /// Pins or unpins a memory; false when the id is unknown.
    ///
    /// # Errors
    ///
    /// I/O errors.
    pub fn set_pinned(&self, id: &str, pinned: bool) -> std::io::Result<bool> {
        if !self.load().iter().any(|m| m.id == id) {
            return Ok(false);
        }
        self.append(&[Record::Pin {
            id: id.to_owned(),
            at: now_ms(),
            pinned,
        }])?;
        Ok(true)
    }
}
