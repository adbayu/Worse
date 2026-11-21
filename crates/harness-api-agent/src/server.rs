use super::{
    background, mpsc, Agent, Arc, AsyncBufReadExt, AsyncWriteExt, AtomicBool, BufReader, HashMap,
    Mutex, Provider, Value,
};

/// Runs the agent on stdio until stdin closes. Entry point of the `harness-api-agent` binary and of
/// `harness-slint --api-agent` (phase-9-parity.md §9.9).
#[must_use]
pub fn run() -> std::process::ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("harness-api-agent: runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve())
}

pub(super) async fn serve() -> std::process::ExitCode {
    // reqwest 使用 rustls-no-provider：进程内先装 ring 作为默认加密实现，否则 HTTPS 请求会失败。
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (out, mut out_rx) = mpsc::unbounded_channel::<String>();
    let writer = tokio::spawn(async move {
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                break;
            }
        }
    });
    let http = match reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(30))
        .user_agent(concat!("harness-api-agent/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(error) => {
            eprintln!("harness-api-agent: http client: {error}");
            return std::process::ExitCode::from(2);
        }
    };
    let agent = Arc::new_cyclic(|me| Agent {
        provider: Provider::from_env(),
        prompts: Box::new(crate::prompt_source::ConfigPrompts),
        http,
        out,
        pending: Mutex::new(HashMap::new()),
        next_id: Mutex::new(0),
        sessions: Mutex::new(HashMap::new()),
        cancels: Mutex::new(HashMap::new()),
        me: me.clone(),
        jobs: background::Jobs::default(),
        async_tasks: AtomicBool::new(false),
        echo_reasoning: AtomicBool::new(true),
        cache_control: AtomicBool::new(true),
        vision: AtomicBool::new(true),
        system_mode: std::sync::atomic::AtomicU8::new(0),
        system_probe: tokio::sync::Mutex::new(()),
        gateway_mode: std::sync::atomic::AtomicU8::new(0),
    });
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        agent.dispatch(&message);
    }
    // stdin closed: the hub is shutting us down. Stop every turn (kills running commands).
    agent.cancel_all();
    drop(agent);
    let _ = writer.await;
    std::process::ExitCode::SUCCESS
}

pub(super) fn text_param(params: &Value, key: &str) -> String {
    params
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}
