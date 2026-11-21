use super::*;

fn candidates() -> Vec<Candidate> {
    vec![
        Candidate {
            id: "api-deepseek".into(),
            name: "DeepSeek".into(),
            role: "Subagent".into(),
            note: "cheap, for mechanical edits".into(),
            models: Vec::new(),
        },
        Candidate {
            id: "command-code-key".into(),
            name: "Command Code · Key".into(),
            role: "QC / Reviewer".into(),
            note: String::new(),
            models: vec![
                (
                    "deepseek/deepseek-v4-flash".into(),
                    "DeepSeek V4 Flash".into(),
                ),
                ("gpt-5.5".into(), "GPT 5.5".into()),
            ],
        },
    ]
}

#[test]
fn the_question_lists_agents_roles_notes_and_models() {
    let body = request_body(
        "small-model",
        "rename a variable in src/lib.rs",
        &candidates(),
    );
    assert_eq!(body["model"], "small-model");
    assert_eq!(body["temperature"], 0);
    let question = body["messages"][1]["content"].as_str().unwrap();
    assert!(question.starts_with("Task:\nrename a variable in src/lib.rs\n\nAgents:\n"));
    assert!(question.contains(
        "- id: api-deepseek; name: DeepSeek; role: Subagent; notes: cheap, for mechanical edits\n"
    ));
    assert!(question.contains("- id: command-code-key; name: Command Code · Key; role: QC / Reviewer; models: deepseek/deepseek-v4-flash, gpt-5.5\n"));
    let long = "x".repeat(10_000);
    let body = request_body("m", &long, &candidates());
    assert!(body["messages"][1]["content"].as_str().unwrap().len() < 3_400);
}

#[test]
fn answers_are_checked_against_the_candidates() {
    let c = candidates();
    let plain = parse(
        r#"{"agent":"api-deepseek","model":"","reason":"a small edit"}"#,
        &c,
    )
    .unwrap();
    assert_eq!(
        plain,
        Choice {
            agent: "api-deepseek".into(),
            model: None,
            reason: "a small edit".into()
        }
    );
    // Wrapped in prose and a code fence; the model named by its display name.
    let fenced = parse("Sure.\n```json\n{\"agent\": \"command-code-key\", \"model\": \"gpt 5.5\", \"reason\": \"needs\\n review\"}\n```", &c).unwrap();
    assert_eq!(fenced.agent, "command-code-key");
    assert_eq!(fenced.model.as_deref(), Some("gpt-5.5"));
    assert_eq!(fenced.reason, "needs review");
    // The agent named by its display name.
    assert_eq!(
        parse(r#"{"agent":"deepseek"}"#, &c).unwrap().agent,
        "api-deepseek"
    );
    // A model the agent does not offer is dropped; an unknown agent is no answer.
    assert_eq!(
        parse(r#"{"agent":"command-code-key","model":"claude-9"}"#, &c)
            .unwrap()
            .model,
        None
    );
    assert!(parse(r#"{"agent":"gemini"}"#, &c).is_none());
    assert!(parse("I would pick DeepSeek.", &c).is_none());
    assert!(parse("", &c).is_none());
}

#[test]
fn config_round_trips_and_defaults_to_off() {
    let home = std::env::temp_dir().join(format!(
        "harness-decision-{}",
        uuid::Uuid::new_v4().simple()
    ));
    assert_eq!(load(&home), Config::default());
    assert!(!load(&home).active());
    let config = Config {
        provider: "api-openrouter".into(),
        model: "small".into(),
        delegation: true,
    };
    save(&home, &config).unwrap();
    assert_eq!(load(&home), config);
    assert!(load(&home).active());
    let _ = std::fs::remove_dir_all(&home);
}

/// The whole call against a local Chat Completions server: the request carries the key and the question,
/// and the answer becomes a choice. Then the cases that leave the decision to the rule.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(clippy::too_many_lines)]
async fn choose_asks_the_provider_and_falls_back_cleanly() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let home = std::env::temp_dir().join(format!(
        "harness-decision-{}",
        uuid::Uuid::new_v4().simple()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let seen = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let log = std::sync::Arc::clone(&seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = vec![0u8; 65_536];
            let mut request = String::new();
            loop {
                let n = socket.read(&mut buffer).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                request.push_str(&String::from_utf8_lossy(&buffer[..n]));
                if let Some(head) = request.find("\r\n\r\n") {
                    let length: usize = request[..head]
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if request.len() >= head + 4 + length {
                        break;
                    }
                }
            }
            *log.lock().unwrap() = request.clone();
            let content = if request.contains("please-fail") {
                "no idea"
            } else {
                "{\"agent\":\"command-code-key\",\"model\":\"gpt-5.5\",\"reason\":\"review work\"}"
            };
            let body =
                serde_json::json!({"choices":[{"message":{"role":"assistant","content":content}}]})
                    .to_string();
            let reply = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    std::fs::write(
        home.join("providers.json"),
        format!(r#"{{"providers":[{{"id":"api-router","label":"Router","baseUrl":"http://127.0.0.1:{port}/v1","model":"provider-model","apiKey":"router-key"}}]}}"#),
    ).unwrap();
    // Off: no request.
    assert!(choose(&home, "task", &candidates()).await.is_err());
    assert!(seen.lock().unwrap().is_empty());
    save(
        &home,
        &Config {
            provider: "api-router".into(),
            model: "tiny".into(),
            delegation: true,
        },
    )
    .unwrap();
    let choice = choose(&home, "review the auth change", &candidates())
        .await
        .unwrap();
    assert_eq!(
        choice,
        Choice {
            agent: "command-code-key".into(),
            model: Some("gpt-5.5".into()),
            reason: "review work".into()
        }
    );
    let request = seen.lock().unwrap().clone();
    assert!(
        request.starts_with("POST /v1/chat/completions"),
        "{request}"
    );
    assert!(request
        .to_ascii_lowercase()
        .contains("authorization: bearer router-key"));
    assert!(request.contains("\"model\":\"tiny\"") && request.contains("review the auth change"));
    // An answer that names no agent, a provider that is gone, a switch that is off: the rule decides.
    assert!(choose(&home, "please-fail", &candidates()).await.is_err());
    save(
        &home,
        &Config {
            provider: "api-gone".into(),
            ..Config::default()
        },
    )
    .unwrap();
    assert!(choose(&home, "task", &candidates())
        .await
        .unwrap_err()
        .contains("not configured"));
    save(
        &home,
        &Config {
            provider: "api-router".into(),
            model: String::new(),
            delegation: false,
        },
    )
    .unwrap();
    assert!(choose(&home, "task", &candidates()).await.is_err());
    let _ = std::fs::remove_dir_all(&home);
}
