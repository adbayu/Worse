use super::{
    mpsc, Arc, AtomicU64, Mutex, Ordering, PoisonError, WorkspaceEvent, WorkspaceEventKind,
};

/// One subscriber: its channel and its own seq counter.
pub(super) type Subscriber = (mpsc::UnboundedSender<WorkspaceEvent>, Arc<AtomicU64>);

/// Fan-out of workspace events: one unbounded channel per subscriber, so no event is ever dropped.
#[derive(Debug, Default, Clone)]
pub struct WorkspaceEventBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

impl WorkspaceEventBus {
    /// New bus without subscribers.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Delivers `kind` to every live subscriber (closed subscribers are dropped).
    pub fn emit(&self, kind: &WorkspaceEventKind) {
        let mut subscribers = self
            .subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        subscribers.retain(|(tx, seq)| {
            let event = WorkspaceEvent {
                seq: seq.fetch_add(1, Ordering::SeqCst) + 1,
                kind: kind.clone(),
            };
            tx.send(event).is_ok()
        });
    }

    /// New subscription from now on.
    #[must_use]
    pub fn subscribe(&self) -> WorkspaceEventStream {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((tx, Arc::new(AtomicU64::new(0))));
        WorkspaceEventStream { inner: rx }
    }

    /// Ends every subscription (their `recv` returns `None` after the queued events).
    pub fn close(&self) {
        self.subscribers
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

/// Subscription to workspace events.
#[derive(Debug)]
pub struct WorkspaceEventStream {
    inner: mpsc::UnboundedReceiver<WorkspaceEvent>,
}

impl WorkspaceEventStream {
    /// Next event; `None` after the workspace shut down and the queue drained.
    pub async fn recv(&mut self) -> Option<WorkspaceEvent> {
        self.inner.recv().await
    }

    /// Non-blocking receive.
    pub fn try_recv(&mut self) -> Option<WorkspaceEvent> {
        self.inner.try_recv().ok()
    }
}
