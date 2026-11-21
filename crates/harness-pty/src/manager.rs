use super::{
    default_env, exit_code, finish_session, lock, mpsc, native_pty_system, read_loop, resolve_cwd,
    resolve_shell, utf16_len, windows_pty_info, Arc, AtomicU64, CommandBuilder, Condvar,
    CreateOptions, Created, HashMap, Instant, ManagerInner, Mutex, Ordering, OutState, PtyConfig,
    PtyError, PtyEvent, PtyManager, PtySink, PtySize, ReaderSignal, Session, TreeGuard, Write,
    BAD_SIZE_MESSAGE,
};

impl PtyManager {
    /// New manager.
    #[must_use]
    pub fn new(config: PtyConfig) -> Self {
        Self {
            inner: Arc::new(ManagerInner {
                config,
                sessions: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(0),
            }),
        }
    }

    /// Spawns a shell on a new PTY.
    ///
    /// # Errors
    /// [`BAD_SIZE_MESSAGE`], the TS no-shell / no-cwd texts, or
    /// `Failed to start terminal with shell '<shell>' in '<cwd>': <reason>`.
    #[allow(clippy::too_many_lines)] // 线性流程：校验 → spawn → 收容进程树 → 启动读/等待线程。
    pub fn create(&self, options: CreateOptions, sink: PtySink) -> Result<Created, PtyError> {
        if options.cols == 0 || options.rows == 0 {
            return Err(PtyError::App(BAD_SIZE_MESSAGE.to_owned()));
        }
        let mut env = options.env.unwrap_or_else(default_env);
        let shell = match options.shell {
            Some(shell) => shell,
            None => resolve_shell(&env).map_err(PtyError::App)?,
        };
        let cwd = match options.cwd {
            Some(cwd) => cwd,
            None => resolve_cwd(None, &env).map_err(PtyError::App)?,
        };
        if cfg!(not(windows)) {
            // node-pty（Unix）把 name 写入子进程的 TERM。
            if let Some(name) = &options.name {
                env.set("TERM", name);
            }
        }
        let fail = |reason: String| {
            PtyError::App(format!(
                "Failed to start terminal with shell '{shell}' in '{cwd}': {reason}"
            ))
        };
        let size = PtySize {
            rows: options.rows,
            cols: options.cols,
            pixel_width: 0,
            pixel_height: 0,
        };
        let pair = native_pty_system()
            .openpty(size)
            .map_err(|e| fail(e.to_string()))?;
        let mut command = CommandBuilder::new(&shell);
        command.args(&options.args);
        command.cwd(&cwd);
        command.env_clear();
        for (key, value) in env.iter() {
            command.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(command)
            .map_err(|e| fail(e.to_string()))?;
        let tree = TreeGuard::contain(child.as_ref());
        // 只保留 master：slave 端句柄留在本进程会导致 shell 退出后读端永远等不到 EOF。
        drop(pair.slave);
        let pid = child.process_id();
        let id = self
            .inner
            .next_id
            .fetch_add(1, Ordering::Relaxed)
            .to_string();
        let killer = child.clone_killer();
        let reader = pair.master.try_clone_reader();
        let writer = pair.master.take_writer();
        let (reader, writer) = match (reader, writer) {
            (Ok(reader), Ok(writer)) => (reader, writer),
            (Err(error), _) | (_, Err(error)) => {
                let mut tree = tree;
                tree.force();
                let _ = child.kill();
                let _ = child.wait();
                return Err(fail(error.to_string()));
            }
        };
        let session = Arc::new(Session {
            id: id.clone(),
            sink,
            writer: Mutex::new(Some(writer)),
            master: Mutex::new(Some(pair.master)),
            killer: Mutex::new(killer),
            tree: Mutex::new(tree),
            out: Mutex::new(OutState {
                attached: options.attached,
                flow_control: options.flow_control,
                ..OutState::default()
            }),
            flow: Condvar::new(),
            exited: Mutex::new(false),
            exited_cv: Condvar::new(),
        });
        lock(&self.inner.sessions).insert(id.clone(), Arc::clone(&session));

        let config = self.inner.config;
        let (activity_tx, activity_rx) = mpsc::channel::<ReaderSignal>();
        let reader_session = Arc::clone(&session);
        let spawned_reader = std::thread::Builder::new()
            .name(format!("harness-pty-read-{id}"))
            .spawn(move || read_loop(&reader_session, &config, reader, &activity_tx));
        let waiter_session = Arc::clone(&session);
        let manager = Arc::downgrade(&self.inner);
        let spawned_waiter = std::thread::Builder::new()
            .name(format!("harness-pty-wait-{id}"))
            .spawn(move || {
                let code = match child.wait() {
                    Ok(status) => exit_code(&status),
                    Err(error) => {
                        tracing::warn!(%error, "pty wait failed");
                        -1
                    }
                };
                waiter_session.mark_exited();
                finish_session(&manager, &waiter_session, &config, code, &activity_rx);
            });
        if let Err(error) = spawned_reader.and(spawned_waiter) {
            self.kill(&id);
            return Err(fail(error.to_string()));
        }
        tracing::info!(id, shell, cwd, ?pid, "terminal created");
        Ok(Created {
            id,
            shell,
            cwd,
            pid,
            windows_pty: windows_pty_info(),
        })
    }

    pub(super) fn get(&self, id: &str) -> Result<Arc<Session>, PtyError> {
        lock(&self.inner.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| PtyError::NotFound(id.to_owned()))
    }

    /// Writes raw input.
    ///
    /// # Errors
    /// `NotFound`, or the write error text.
    pub fn write(&self, id: &str, data: &[u8]) -> Result<(), PtyError> {
        let session = self.get(id)?;
        let mut writer = lock(&session.writer);
        let Some(writer) = writer.as_mut() else {
            return Err(PtyError::NotFound(id.to_owned()));
        };
        writer
            .write_all(data)
            .and_then(|()| writer.flush())
            .map_err(|e| PtyError::App(e.to_string()))
    }

    /// Resizes the PTY.
    ///
    /// # Errors
    /// `NotFound`, [`BAD_SIZE_MESSAGE`], or the platform error text.
    pub fn resize(&self, id: &str, cols: u16, rows: u16) -> Result<(), PtyError> {
        let session = self.get(id)?;
        if cols == 0 || rows == 0 {
            return Err(PtyError::App(BAD_SIZE_MESSAGE.to_owned()));
        }
        let master = lock(&session.master);
        let Some(master) = master.as_ref() else {
            return Err(PtyError::NotFound(id.to_owned()));
        };
        master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| PtyError::App(e.to_string()))
    }

    /// Starts streaming a detached session: replays buffered output in
    /// order, then goes live.
    ///
    /// # Errors
    /// `NotFound`.
    pub fn attach(&self, id: &str) -> Result<(), PtyError> {
        let session = self.get(id)?;
        let mut out = lock(&session.out);
        if out.attached {
            return Ok(());
        }
        out.attached = true;
        out.pending_units = 0;
        let pending: Vec<PtyEvent> = out.pending.drain(..).collect();
        let mut exited = false;
        for event in pending {
            match &event {
                PtyEvent::Data(text) if out.flow_control => out.unacked += utf16_len(text),
                PtyEvent::Exit(_) => exited = true,
                PtyEvent::Data(_) => {}
            }
            (session.sink)(&session.id, event);
        }
        if exited {
            out.silenced = true;
        }
        session.flow.notify_all();
        drop(out);
        if exited {
            lock(&self.inner.sessions).remove(id);
        }
        Ok(())
    }

    /// Flow control: the consumer processed `units` UTF-16 units.
    ///
    /// # Errors
    /// `NotFound`.
    pub fn ack(&self, id: &str, units: usize) -> Result<(), PtyError> {
        let session = self.get(id)?;
        let mut out = lock(&session.out);
        out.unacked = out.unacked.saturating_sub(units);
        session.flow.notify_all();
        Ok(())
    }

    /// Kills the session's whole process tree and reaps it. No event is
    /// emitted for the session afterwards (TS disposes its emitters). Unknown
    /// ids are a no-op; returns whether the id was active.
    pub fn kill(&self, id: &str) -> bool {
        let Some(session) = lock(&self.inner.sessions).remove(id) else {
            return false;
        };
        session.begin_kill();
        session.finish_kill(Instant::now() + self.inner.config.kill_grace);
        tracing::info!(id, "terminal killed");
        true
    }

    /// Kills every session (host shutdown). The hang-up goes to all trees
    /// first, so the grace period is shared instead of serial.
    pub fn dispose_all(&self) {
        let sessions: Vec<Arc<Session>> =
            lock(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        for session in &sessions {
            session.begin_kill();
        }
        let deadline = Instant::now() + self.inner.config.kill_grace;
        for session in &sessions {
            session.finish_kill(deadline);
        }
    }

    /// Active session ids.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        lock(&self.inner.sessions).keys().cloned().collect()
    }
}
