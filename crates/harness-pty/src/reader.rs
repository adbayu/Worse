use super::{
    lock, mpsc, Arc, ManagerInner, PtyConfig, PtyEvent, Read, Session, Utf8Stream, Weak, Write,
};

/// node-pty exit codes: Unix `WEXITSTATUS`, `0` when killed by a signal;
/// Windows the unsigned `GetExitCodeProcess` value.
pub(super) fn exit_code(status: &portable_pty::ExitStatus) -> i64 {
    if status.signal().is_some() {
        0
    } else {
        i64::from(status.exit_code())
    }
}

pub(super) enum ReaderSignal {
    Data,
    Eof,
}

/// `ConPTY`'s startup cursor query.
pub(super) const STARTUP_DSR: &str = "\x1b[6n";
/// Cursor at the origin: a fresh terminal, like node-pty (which does not ask
/// `ConPTY` to inherit the cursor).
pub(super) const STARTUP_DSR_REPLY: &str = "\x1b[1;1R";

pub(super) enum DsrStep {
    NeedMore,
    Answered(String),
    NotDsr(String),
}

/// portable-pty 固定以 `PSEUDOCONSOLE_INHERIT_CURSOR` 创建 ConPTY，conhost 启动时先发
/// `ESC[6n` 并阻塞全部输出直到收到光标位置回复；node-pty 不带该标志。这里只拦截启动时的
/// 第一个查询并回复 (1,1)，与 node-pty 的新终端语义一致；之后程序发出的查询照常交给客户端。
pub(super) struct PendingDsr {
    head: String,
}

impl PendingDsr {
    pub(super) fn new() -> Option<Self> {
        cfg!(windows).then(|| Self {
            head: String::new(),
        })
    }

    pub(super) fn filter(&mut self, text: &str) -> DsrStep {
        self.head.push_str(text);
        if let Some(rest) = self.head.strip_prefix(STARTUP_DSR) {
            return DsrStep::Answered(rest.to_owned());
        }
        if STARTUP_DSR.starts_with(self.head.as_str()) {
            return DsrStep::NeedMore;
        }
        DsrStep::NotDsr(std::mem::take(&mut self.head))
    }
}

pub(super) fn read_loop(
    session: &Session,
    config: &PtyConfig,
    mut reader: Box<dyn Read + Send>,
    activity: &mpsc::Sender<ReaderSignal>,
) {
    let mut buf = vec![0u8; config.read_chunk.max(1024)];
    let mut decoder = Utf8Stream::default();
    let mut startup = PendingDsr::new();
    loop {
        session.wait_for_credit(config);
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                let mut text = decoder.push(&buf[..n]);
                if let Some(pending) = startup.as_mut() {
                    match pending.filter(&text) {
                        DsrStep::NeedMore => continue,
                        DsrStep::Answered(rest) => {
                            startup = None;
                            if let Some(writer) = lock(&session.writer).as_mut() {
                                let _ = writer
                                    .write_all(STARTUP_DSR_REPLY.as_bytes())
                                    .and_then(|()| writer.flush());
                            }
                            text = rest;
                        }
                        DsrStep::NotDsr(all) => {
                            startup = None;
                            text = all;
                        }
                    }
                }
                if !text.is_empty() {
                    session.emit(config, PtyEvent::Data(text));
                    let _ = activity.send(ReaderSignal::Data);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            // Unix PTY 在所有 slave 关闭后返回 EIO，这是正常的流结束。
            Err(_) => break,
        }
    }
    let tail = decoder.finish();
    if !tail.is_empty() {
        session.emit(config, PtyEvent::Data(tail));
    }
    let _ = activity.send(ReaderSignal::Eof);
}

pub(super) fn finish_session(
    manager: &Weak<ManagerInner>,
    session: &Arc<Session>,
    config: &PtyConfig,
    code: i64,
    activity: &mpsc::Receiver<ReaderSignal>,
) {
    // Windows ConPTY 在伪控制台关闭前不会给读端 EOF；先关 PTY，再把剩余输出转发到 EOF
    // 或 200ms 无数据为止（后台进程可能一直持有 Unix slave），保证 Exit 排在全部 Data 之后。
    session.close_pty();
    loop {
        match activity.recv_timeout(config.exit_idle) {
            Ok(ReaderSignal::Data) => {}
            Ok(ReaderSignal::Eof) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                // 流控暂停时读线程并非空闲，而是在等消费者确认，不能据此提前结束。
                let out = lock(&session.out);
                let paused = out.flow_control
                    && out.attached
                    && !out.silenced
                    && out.unacked >= config.flow_high_water;
                if !paused {
                    break;
                }
            }
        }
    }
    session.emit(config, PtyEvent::Exit(code));
    {
        let mut out = lock(&session.out);
        if !out.attached && !out.silenced {
            // 未订阅时 Exit 留在缓冲区等待 attach 回放，会话保留以便 attach。
            return;
        }
        out.silenced = true;
    }
    // 退出后进程树里可能仍有后台进程：与 kill 同样回收（Windows 关闭 Job，Unix 挂断进程组）。
    lock(&session.tree).hang_up();
    if let Some(manager) = manager.upgrade() {
        let mut sessions = lock(&manager.sessions);
        if sessions
            .get(&session.id)
            .is_some_and(|s| Arc::ptr_eq(s, session))
        {
            sessions.remove(&session.id);
        }
    }
    tracing::info!(id = session.id, code, "terminal exited");
}
