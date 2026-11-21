use super::{
    mpsc, thread, Arc, AtomicUsize, Child, ChildStderr, ChildStdout, Command, Mutex, OnceLock,
    Ordering, PoisonError, Read, Stdio,
};

/// One child pipe handed to a pooled reader.
pub(super) enum Pipe {
    Stdout(ChildStdout),
    Stderr(ChildStderr),
}

/// Reader -> waiter notifications.
pub(super) enum ReaderEvent {
    /// The stream hit its cap (sent once, before `Done`); the waiter kills the child.
    Truncated,
    /// EOF with the captured bytes; `true` for stdout.
    Done(bool, Vec<u8>),
}

pub(super) type Job = Box<dyn FnOnce() + Send + 'static>;

/// Process-wide reader pool: idle workers are reused, a new worker is only
/// started when every existing one is busy (peak concurrency is small).
pub(super) struct ReaderPool {
    sender: Mutex<mpsc::Sender<Job>>,
    receiver: Arc<Mutex<mpsc::Receiver<Job>>>,
    idle: AtomicUsize,
}

pub(super) fn reader_pool() -> &'static ReaderPool {
    static POOL: OnceLock<ReaderPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        ReaderPool {
            sender: Mutex::new(sender),
            receiver: Arc::new(Mutex::new(receiver)),
            idle: AtomicUsize::new(0),
        }
    })
}

impl ReaderPool {
    pub(super) fn execute(&'static self, job: Job) {
        // 预约一个空闲 worker；成功则投递到共享队列，否则新建 worker 直接执行该任务。
        let mut idle = self.idle.load(Ordering::Acquire);
        let reserved = loop {
            if idle == 0 {
                break false;
            }
            match self.idle.compare_exchange_weak(
                idle,
                idle - 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break true,
                Err(current) => idle = current,
            }
        };
        if reserved {
            let sent = self
                .sender
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .send(job);
            if let Err(mpsc::SendError(job)) = sent {
                job();
            }
            return;
        }
        let spawned = thread::Builder::new()
            .name("git-output".to_owned())
            .spawn(move || {
                job();
                loop {
                    self.idle.fetch_add(1, Ordering::AcqRel);
                    let next = self
                        .receiver
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .recv();
                    match next {
                        Ok(job) => job(),
                        Err(_) => return,
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "git reader thread spawn failed");
        }
    }
}

pub(super) fn submit_reader(pipe: Pipe, max: usize, events: mpsc::Sender<ReaderEvent>) {
    reader_pool().execute(Box::new(move || {
        let (is_stdout, out) = match pipe {
            Pipe::Stdout(stream) => (true, read_capped(stream, max, &events)),
            Pipe::Stderr(stream) => (false, read_capped(stream, max, &events)),
        };
        let _ = events.send(ReaderEvent::Done(is_stdout, out));
    }));
}

/// Reads to EOF keeping at most `max` bytes; past the cap the stream is
/// drained (so the child never blocks on a full pipe) and `Truncated` is sent.
pub(super) fn read_capped<R: Read>(
    mut stream: R,
    max: usize,
    events: &mpsc::Sender<ReaderEvent>,
) -> Vec<u8> {
    let mut out = Vec::new();
    let mut truncated = false;
    let mut buf = [0u8; 16 * 1024];
    loop {
        match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if truncated {
                    continue;
                }
                if out.len() + n > max {
                    truncated = true;
                    let _ = events.send(ReaderEvent::Truncated);
                    continue;
                }
                out.extend_from_slice(&buf[..n]);
            }
        }
    }
    out
}

/// Kills the child; on Windows the whole tree (git helpers can hold pipes).
pub(super) fn kill_tree(child: &mut Child) {
    #[cfg(windows)]
    {
        let mut taskkill = Command::new("taskkill");
        taskkill
            .args(["/PID", &child.id().to_string(), "/T", "/F"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        {
            use std::os::windows::process::CommandExt;
            taskkill.creation_flags(0x0800_0000);
        }
        let _ = taskkill.status();
    }
    let _ = child.kill();
}
