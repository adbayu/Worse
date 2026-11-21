use super::{
    lock, Arc, CloseReason, EventKind, FileWatchEvent, HubInner, Identity, Instant, ModifyKind,
    Path, PathBuf, SchedMsg, Subscription, WatchSignal,
};

/// Marks the subscription closed and drops its platform watcher outside of
/// every lock (dropping may join the backend thread, which may be waiting on
/// `state`).
pub(super) fn close_subscription(sub: &Subscription) {
    lock(&sub.state).closed = true;
    let watcher = lock(&sub.watcher).take();
    drop(watcher);
    tracing::debug!(id = sub.id, "watch stopped");
}

pub(super) fn rename_like(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(ModifyKind::Name(_))
    )
}

impl Subscription {
    /// TS `resolveFileWatchChangedPath`: `path.resolve(dirPath, fileName)`.
    pub(super) fn locate(&self, raw: &Path) -> Located {
        let raw = PathBuf::from(harness_fs::strip_verbatim(&raw.to_string_lossy()).as_ref());
        for root in &self.roots {
            if let Ok(rel) = raw.strip_prefix(root) {
                let rel = rel.to_string_lossy();
                if rel.is_empty() {
                    return Located::Root;
                }
                let rel = rel.replace('\\', "/");
                let joined = harness_fs::node_join(&self.dir_path, &rel);
                return Located::Child(joined, rel);
            }
        }
        Located::Outside
    }
}

pub(super) enum Located {
    /// The watched directory itself.
    Root,
    /// A child: `(path.resolve(dirPath, rel), rel)`.
    Child(String, String),
    /// Not under any root (a sibling seen through the parent watch, or a
    /// path spelling no root matches).
    Outside,
}

pub(super) fn on_raw_event(
    hub: &HubInner,
    sub: &Subscription,
    result: notify::Result<notify::Event>,
) {
    let event = match result {
        Ok(event) => event,
        Err(error) => {
            // 与 TS 一致：底层 watcher 报错时发一次最终事件并关闭订阅。
            tracing::warn!(id = sub.id, path = sub.dir_path, %error, "watcher error, closing");
            fail_subscription(hub, sub, CloseReason::Error(error.to_string()));
            return;
        }
    };
    let config = hub.config;
    let now = Instant::now();
    let mut state = lock(&sub.state);
    if state.closed {
        return;
    }
    let mut relevant = event.need_rescan();
    if event.need_rescan() {
        state.overflow = true;
        state.unknown = true;
    }
    if event.paths.is_empty() && !relevant {
        state.unknown = true;
        relevant = true;
    }
    for raw in &event.paths {
        match sub.locate(raw) {
            Located::Child(changed, rel) => {
                if sub
                    .ignore
                    .as_ref()
                    .is_some_and(|rules| rules.is_ignored(&rel, raw.is_dir()))
                {
                    continue;
                }
                relevant = true;
                if state.pending.len() >= config.max_pending_paths {
                    state.overflow = true;
                    state.unknown = true;
                } else {
                    state.pending.insert(changed);
                }
            }
            // 根目录自身被删除/改名：只触发身份校验；属性变化等不产生事件（Node 不报告根目录自身）。
            Located::Root => relevant |= rename_like(event.kind),
            Located::Outside if sub.parent_watched => {}
            Located::Outside => {
                relevant = true;
                state.unknown = true;
            }
        }
    }
    if !relevant {
        return;
    }
    if rename_like(event.kind) {
        state.saw_rename = true;
    }
    state.debounce_at = Some(now + config.debounce);
    if state.max_wait_at.is_none() {
        // 与 TS 修复一致：独立的 max-wait 不随事件重置，持续写入时也能周期性通知。
        state.max_wait_at = Some(now + config.max_wait);
    }
    let deadline = state.deadline();
    drop(state);
    if let Some(deadline) = deadline {
        let _ = lock(&hub.sched).send(SchedMsg::Schedule(sub.id.clone(), deadline));
    }
}

/// Emits the final path-less event and `Closed`, then forgets the
/// subscription. Idempotent. Emission happens under the state lock and the
/// removal from the hub after it, so a concurrent `unwatch` either finds the
/// subscription (and waits for the lock) or returns after everything was sent.
pub(super) fn fail_subscription(hub: &HubInner, sub: &Subscription, reason: CloseReason) {
    {
        let mut state = lock(&sub.state);
        if state.closed {
            return;
        }
        state.closed = true;
        state.pending.clear();
        (sub.sink)(
            &sub.id,
            WatchSignal::Change {
                event: FileWatchEvent {
                    dir_path: sub.dir_path.clone(),
                    changed_path: None,
                },
                rescan: false,
            },
        );
        (sub.sink)(&sub.id, WatchSignal::Closed { reason });
    }
    lock(&hub.subs).remove(&sub.id);
    // 不能在 notify 回调线程里同步 drop watcher（可能 join 自身线程），交给调度线程释放。
    let _ = lock(&hub.sched).send(SchedMsg::Release(sub.id.clone(), lock(&sub.watcher).take()));
}

/// Flushes one subscription if its window is due. Called on the scheduler
/// thread. Returns the next deadline when the window is not due yet.
pub(super) fn flush_if_due(
    hub: &HubInner,
    sub: &Arc<Subscription>,
    now: Instant,
) -> Option<Instant> {
    let (pending, unknown, saw_rename, overflow) = {
        let mut state = lock(&sub.state);
        if state.closed {
            return None;
        }
        match state.deadline() {
            None => return None,
            Some(deadline) if deadline > now => return Some(deadline),
            Some(_) => {}
        }
        // 与 TS 一致：先取走本批并开启空批次，校验期间的新事件进入下一批。
        state.debounce_at = None;
        state.max_wait_at = None;
        (
            std::mem::take(&mut state.pending),
            std::mem::replace(&mut state.unknown, false),
            std::mem::replace(&mut state.saw_rename, false),
            std::mem::replace(&mut state.overflow, false),
        )
    };
    // 身份校验在锁外做（stat 可能慢）。
    let identity_ok = !saw_rename
        || match (&sub.identity, Identity::of(Path::new(&sub.dir_path))) {
            (Some(created), Some(current)) => *created == current,
            _ => false,
        };
    if !identity_ok {
        tracing::warn!(
            id = sub.id,
            path = sub.dir_path,
            "watched directory identity changed; final event and close"
        );
        fail_subscription(hub, sub, CloseReason::IdentityChanged);
        return None;
    }
    let state = lock(&sub.state);
    if state.closed {
        return None;
    }
    let changed_path = if !unknown && pending.len() == 1 {
        pending.into_iter().next()
    } else {
        None
    };
    // 在状态锁内发送：unwatch 关闭订阅时会等待该锁，保证 unsubscribe 响应之后不再有事件。
    (sub.sink)(
        &sub.id,
        WatchSignal::Change {
            event: FileWatchEvent {
                dir_path: sub.dir_path.clone(),
                changed_path,
            },
            rescan: overflow,
        },
    );
    drop(state);
    None
}
