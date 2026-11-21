//! The `QuickJS` side of code mode: one thread owns the runtime per run and the host answers its
//! calls over channels. The runtime is collected with `run_gc` before it is dropped, because
//! `JS_FreeRuntime` aborts the process when its object list is not empty.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rquickjs::{Array, Context, Ctx as JsCtx, Error, Function, Object, Promise, Runtime};
use serde_json::json;
use tokio::sync::mpsc::UnboundedSender;

use crate::code_mode_script;

const MEMORY: usize = 256 * 1024 * 1024;
const STACK: usize = 1024 * 1024;
/// Host tool calls a single script may make.
const CALLS_MAX: usize = 50;
/// Largest JSON argument string a single call may pass.
const ARGS_MAX: usize = 200_000;

/// One tool call the script asks the host to perform.
pub struct HostRequest {
    pub name: String,
    pub args: String,
}

/// Everything the script thread needs: the code, both channels, the interrupt flag and the limit.
pub struct Script {
    code: String,
    requests: UnboundedSender<HostRequest>,
    replies: Receiver<String>,
    interrupted: Arc<AtomicBool>,
    calls: Arc<AtomicUsize>,
    limit: Duration,
}

impl Script {
    /// Builds a run; the caller keeps the sending end of `requests` and the receiving end of `replies`.
    pub fn new(
        code: String,
        requests: UnboundedSender<HostRequest>,
        replies: Receiver<String>,
        interrupted: Arc<AtomicBool>,
        limit: Duration,
    ) -> Self {
        Self {
            code,
            requests,
            replies,
            interrupted,
            calls: Arc::new(AtomicUsize::new(0)),
            limit,
        }
    }
}

/// Runs the script to completion and returns its JSON payload (`result`/`error` plus `logs`).
pub fn run(script: Script) -> String {
    let Script {
        code,
        requests,
        replies,
        interrupted,
        calls,
        limit,
    } = script;
    match drive(&code, &requests, &replies, &interrupted, &calls, limit) {
        Ok(payload) => payload,
        Err(error) => json!({"error": error, "logs": []}).to_string(),
    }
}

fn drive(
    code: &str,
    requests: &UnboundedSender<HostRequest>,
    replies: &Receiver<String>,
    interrupted: &Arc<AtomicBool>,
    calls: &Arc<AtomicUsize>,
    limit: Duration,
) -> Result<String, String> {
    let runtime = Runtime::new().map_err(|e| format!("code mode: engine failed to start ({e})"))?;
    runtime.set_memory_limit(MEMORY);
    runtime.set_max_stack_size(STACK);
    let flag = Arc::clone(interrupted);
    runtime.set_interrupt_handler(Some(Box::new(move || flag.load(Ordering::SeqCst))));
    let context = Context::full(&runtime).map_err(|e| format!("code mode: {e}"))?;
    let deadline = Instant::now() + limit;
    let result = context.with(|js| execute(&js, code, requests, replies, deadline, calls, limit));
    drop(context);
    runtime.run_gc();
    result
}

fn execute(
    js: &JsCtx<'_>,
    code: &str,
    requests: &UnboundedSender<HostRequest>,
    replies: &Receiver<String>,
    deadline: Instant,
    calls: &Arc<AtomicUsize>,
    limit: Duration,
) -> Result<String, String> {
    js.eval::<(), _>(code_mode_script::prelude())
        .map_err(|e| error_text(js, &e))?;
    let notify = notify_function(js, requests, calls).map_err(|e| error_text(js, &e))?;
    js.globals()
        .set("__notify", notify)
        .map_err(|e| error_text(js, &e))?;
    let source = format!("globalThis.__run({})", code_mode_script::normalize(code));
    let promise: Promise = js.eval(source).map_err(|e| error_text(js, &e))?;
    let mut served = 0usize;
    loop {
        match promise.finish::<String>() {
            Ok(payload) => return Ok(payload),
            Err(Error::WouldBlock) => {}
            Err(error) => return Err(error_text(js, &error)),
        }
        if served >= pending_len(js)? {
            return Err(
                "code mode: the script stopped without a result (a promise never settled)"
                    .to_owned(),
            );
        }
        let left = deadline.saturating_duration_since(Instant::now());
        let reply = match replies.recv_timeout(left) {
            Ok(reply) => reply,
            Err(RecvTimeoutError::Timeout) => {
                return Err(format!(
                    "code mode: timed out after {} s",
                    limit.as_secs().max(1)
                ))
            }
            Err(RecvTimeoutError::Disconnected) => {
                return Err("code mode: the host stopped answering".to_owned())
            }
        };
        settle(js, served, &reply)?;
        served += 1;
    }
}

fn notify_function<'js>(
    js: &JsCtx<'js>,
    requests: &UnboundedSender<HostRequest>,
    calls: &Arc<AtomicUsize>,
) -> rquickjs::Result<Function<'js>> {
    let requests = requests.clone();
    let calls = Arc::clone(calls);
    Function::new(js.clone(), move |name: String, args: String| -> String {
        if args.len() > ARGS_MAX {
            return "code mode: arguments too large".to_owned();
        }
        if calls.fetch_add(1, Ordering::SeqCst) >= CALLS_MAX {
            return format!("code mode: at most {CALLS_MAX} tool calls per run");
        }
        if requests.send(HostRequest { name, args }).is_err() {
            return "code mode: the host is gone".to_owned();
        }
        String::new()
    })
}

fn pending_len(js: &JsCtx<'_>) -> Result<usize, String> {
    let pending: Array = js
        .globals()
        .get("__pending")
        .map_err(|e| error_text(js, &e))?;
    Ok(pending.len())
}

fn settle(js: &JsCtx<'_>, index: usize, reply: &str) -> Result<(), String> {
    let pending: Array = js
        .globals()
        .get("__pending")
        .map_err(|e| error_text(js, &e))?;
    let slot: Object = pending.get(index).map_err(|e| error_text(js, &e))?;
    let key = if reply.starts_with("Error: ") {
        "reject"
    } else {
        "resolve"
    };
    let function: Function = slot.get(key).map_err(|e| error_text(js, &e))?;
    function
        .call::<_, ()>((reply.to_owned(),))
        .map_err(|e| error_text(js, &e))
}

fn error_text(js: &JsCtx<'_>, error: &Error) -> String {
    if matches!(error, Error::Exception) {
        let caught = js.catch();
        let text = caught.as_exception().map_or_else(
            || "the script threw a value that is not an Error".to_owned(),
            |e| e.message().unwrap_or_default(),
        );
        format!("code mode: {text}")
    } else {
        format!("code mode: {error}")
    }
}

/// Sets the interrupt flag once `limit` has passed; the run sets it early when it is done.
pub fn watch(interrupted: Arc<AtomicBool>, limit: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let start = Instant::now();
        while start.elapsed() < limit {
            if interrupted.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        interrupted.store(true, Ordering::SeqCst);
    })
}
