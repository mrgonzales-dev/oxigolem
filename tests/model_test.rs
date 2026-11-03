use oxigolem::agent::chat::Message;
use oxigolem::provider::ollama::{kv_bytes_per_token, resolve_num_ctx, ModelInfo, Ollama};
use oxigolem::provider::{ChatOptions, Provider};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::time::Duration;

fn test_messages() -> Vec<Message> {
    vec![Message::user("hi")]
}

/// Read a full HTTP request: headers until \r\n\r\n, then body per
/// Content-Length.
fn read_request(stream: &mut std::net::TcpStream) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        let n = stream.read(&mut chunk).unwrap();
        buf.extend_from_slice(&chunk[..n]);
        let s = String::from_utf8_lossy(&buf).to_string();
        if let Some(pos) = s.find("\r\n\r\n") {
            let want = s[..pos]
                .to_lowercase()
                .split("content-length:")
                .nth(1)
                .and_then(|v| v.lines().next())
                .and_then(|v| v.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if s.len() - (pos + 4) >= want {
                return s;
            }
        }
    }
}

/// qwen3:1.7b-shaped model_info blob.
fn qwen3_model_info() -> serde_json::Value {
    serde_json::json!({
        "qwen3.block_count": 28,
        "qwen3.attention.head_count": 16,
        "qwen3.attention.head_count_kv": 8,
        "qwen3.embedding_length": 2048,
        "qwen3.context_length": 40960,
        "qwen3.parameter_count": 1720000000u64
    })
}

#[test]
fn kv_bytes_per_token_qwen3_is_112kib() {
    // 2 (K+V) × 28 layers × 8 kv heads × (2048/16 head dim) × 2 bytes
    // = 2 × 28 × 8 × 128 × 2 = 114_688 = 112 KiB
    let info = qwen3_model_info();
    assert_eq!(kv_bytes_per_token(&info), Some(114_688));
}

#[test]
fn kv_bytes_per_token_missing_fields_is_none() {
    assert_eq!(kv_bytes_per_token(&serde_json::json!({})), None);
}

#[test]
fn num_ctx_env_override_wins() {
    unsafe { std::env::set_var("OXIGOLEM_NUM_CTX", "12345") };
    let got = resolve_num_ctx(40960, 8 << 30, Some(114_688));
    assert_eq!(got, 12345);
    unsafe { std::env::remove_var("OXIGOLEM_NUM_CTX") };
}

#[test]
fn num_ctx_capped_by_ram_budget() {
    unsafe { std::env::remove_var("OXIGOLEM_NUM_CTX") };
    // 1 GiB available, 112 KiB/token, quarter of RAM for KV →
    // (1 GiB / 4) / 114_688 ≈ 2330 → floor kicks in at 8192.
    let got = resolve_num_ctx(40960, 1 << 30, Some(114_688));
    assert_eq!(got, 8192);
    // 16 GiB available → quarter = 4 GiB → ~37_000 tokens, model
    // caps at its own 40960 → min wins but not below floor.
    let got = resolve_num_ctx(40960, 16 << 30, Some(114_688));
    assert_eq!(got, 37_449); // (4 GiB) / 114_688 = 37449.14…
}

#[test]
fn num_ctx_floor_8192_when_ram_tiny() {
    unsafe { std::env::remove_var("OXIGOLEM_NUM_CTX") };
    let got = resolve_num_ctx(40960, 128 << 20, Some(114_688));
    assert_eq!(got, 8192);
}

#[test]
fn num_ctx_unknown_kv_uses_floor() {
    unsafe { std::env::remove_var("OXIGOLEM_NUM_CTX") };
    // No KV info → conservative floor, not the model max.
    let got = resolve_num_ctx(40960, 64 << 30, None);
    assert_eq!(got, 8192);
}

#[test]
fn inspect_parses_model_info_and_caches() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (hits_tx, hits_rx) = mpsc::channel::<()>();
    std::thread::spawn(move || {
        for _ in 0..2 {
            let Ok((mut stream, _)) = listener.accept() else {
                break;
            };
            let req = read_request(&mut stream);
            assert!(req.contains("/api/show"), "got: {req}");
            let mut mi = qwen3_model_info();
            mi.as_object_mut().unwrap().insert(
                "capabilities".into(),
                serde_json::json!(["completion", "tools", "thinking"]),
            );
            let body = serde_json::json!({
                "model_info": mi,
                "capabilities": ["completion", "tools", "thinking"],
            })
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(resp.as_bytes()).unwrap();
            hits_tx.send(()).unwrap();
        }
    });
    let ollama = Ollama::new(format!("http://127.0.0.1:{port}"));
    // Point the disk cache at a throwaway dir so the test doesn't
    // touch ./.oxigolem in the repo.
    let dir = std::env::temp_dir().join(format!("oxi-modelcache-{}", std::process::id()));
    unsafe { std::env::set_var("OXIGOLEM_CONFIG_DIR", &dir) };

    let info = ollama.inspect("qwen3:1.7b").expect("inspect works");
    assert_eq!(info.context_length, 40960);
    assert_eq!(info.capabilities, vec!["completion", "tools", "thinking"]);
    assert_eq!(info.parameter_count, 1_720_000_000);
    assert!(info.supports("tools"));
    assert!(info.supports("thinking"));
    assert!(!info.supports("vision"));

    // Second call hits the in-memory (or disk) cache — no new request.
    let info2 = ollama.inspect("qwen3:1.7b").expect("cached inspect");
    assert_eq!(info2.context_length, 40960);
    assert!(
        hits_rx.recv_timeout(Duration::from_millis(300)).is_ok(),
        "first inspect hits the server"
    );
    assert!(
        hits_rx.recv_timeout(Duration::from_millis(500)).is_err(),
        "second inspect must not hit the server again"
    );

    // Disk cache written — a fresh Ollama instance reads it without
    // hitting the (now-dead) server.
    drop(ollama);
    let ollama2 = Ollama::new(format!("http://127.0.0.1:1")); // dead port
    let info3 = ollama2.inspect("qwen3:1.7b").expect("disk-cached inspect");
    assert_eq!(info3.context_length, 40960);
    unsafe { std::env::remove_var("OXIGOLEM_CONFIG_DIR") };
}

#[test]
fn chat_request_has_options_keep_alive_and_think() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request(&mut stream);
        let body = "{\"message\":{\"content\":\"ok\"}}\n{\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        tx.send(req).unwrap();
    });
    let provider = Ollama::new(format!("http://127.0.0.1:{port}"));
    let opts = ChatOptions {
        num_ctx: 16384,
        think: false,
    };
    provider
        .chat("m", &test_messages(), &[], &opts, &|_| {})
        .unwrap();
    let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let json_start = req.find('{').unwrap();
    let v: serde_json::Value = serde_json::from_str(&req[json_start..]).unwrap();
    assert_eq!(v["options"]["num_ctx"], 16384, "req: {req}");
    assert_eq!(v["options"]["num_predict"], 2048);
    assert_eq!(v["options"]["temperature"], 0.1);
    assert_eq!(v["options"]["top_p"], 0.9);
    assert_eq!(v["keep_alive"], "30m");
    assert_eq!(v["think"], false);
}

#[test]
fn chat_options_think_true_serializes() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (tx, rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let req = read_request(&mut stream);
        let body = "{\"message\":{\"content\":\"ok\"}}\n{\"done\":true}\n";
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(resp.as_bytes()).unwrap();
        tx.send(req).unwrap();
    });
    let provider = Ollama::new(format!("http://127.0.0.1:{port}"));
    let opts = ChatOptions {
        num_ctx: 8192,
        think: true,
    };
    provider
        .chat("m", &test_messages(), &[], &opts, &|_| {})
        .unwrap();
    let req = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&req[req.find('{').unwrap()..]).unwrap();
    assert_eq!(v["think"], true);
}

#[test]
fn think_default_off_when_model_supports_thinking() {
    let info = ModelInfo {
        context_length: 4096,
        capabilities: vec!["completion".into(), "tools".into(), "thinking".into()],
        parameter_count: 0,
        raw: serde_json::json!({}),
    };
    assert!(!info.think_default());
    // A non-thinking model never gets think:true.
    let info2 = ModelInfo {
        context_length: 4096,
        capabilities: vec!["completion".into(), "tools".into()],
        parameter_count: 0,
        raw: serde_json::json!({}),
    };
    assert!(!info2.think_default());
}

#[test]
fn think_override_from_config_json() {
    // .oxigolem/config.json {"models": {"m1": {"think": true}}}
    let dir = std::env::temp_dir().join(format!("oxi-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("config.json"),
        r#"{"models": {"m1": {"think": true}}}"#,
    )
    .unwrap();
    unsafe { std::env::set_var("OXIGOLEM_CONFIG_DIR", &dir) };
    let info = ModelInfo {
        context_length: 4096,
        capabilities: vec!["thinking".into()],
        parameter_count: 0,
        raw: serde_json::json!({}),
    };
    assert!(info.think_for("m1"));
    assert!(!info.think_for("other"));
    unsafe { std::env::remove_var("OXIGOLEM_CONFIG_DIR") };
}
