//! Phase 7: regression measurement against a real Ollama server.
//!
//! `cargo test --test local_perf -- --ignored --nocapture`
//!
//! Requires `ollama serve` on localhost:11434 with the model named
//! in `OXI_PERF_MODEL` (default `qwen3:1.7b`). Prints a 10-turn
//! session table: per-turn prompt tokens, cached %, prefill ms,
//! generation t/s, and load ms. Run before and after a perf change
//! to get a comparable before/after.

use std::sync::Arc;
use std::time::Duration;

use oxigolem::{
    agent::chat::{ChatEngine, ChatEvent},
    provider::{ChatOptions, Provider, Usage, ollama::Ollama},
};

fn ollama_up() -> bool {
    std::net::TcpStream::connect("127.0.0.1:11434").is_ok()
}

/// Drain ChatEvents until Done/Error/Cancelled; return last Usage.
fn run_turn(engine: &mut ChatEngine, prompt: &str) -> Option<Usage> {
    engine.send(prompt.to_string()).expect("send");
    let mut usage = None;
    for _ in 0..2400 {
        // 2400 × 100 ms = 4 min cap per turn
        for ev in engine.poll() {
            match ev {
                ChatEvent::Usage(u) => usage = Some(u),
                ChatEvent::Done(msgs) => {
                    engine.record_appended(msgs);
                    return usage;
                }
                ChatEvent::Error(e) => panic!("turn failed: {e}"),
                ChatEvent::Cancelled => return usage,
                _ => {}
            }
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("turn timed out");
}

#[test]
#[ignore]
fn ten_turn_session_perf_table() {
    if !ollama_up() {
        eprintln!("ollama not running — skipping");
        return;
    }
    let model = std::env::var("OXI_PERF_MODEL").unwrap_or_else(|_| "qwen3:1.7b".into());
    let provider: Arc<dyn Provider> = Arc::new(Ollama::default());
    let mut engine = ChatEngine::new(Arc::clone(&provider), model.clone());

    // Same option resolution path the app uses.
    if let Some(ollama) = provider.as_any().downcast_ref::<Ollama>() {
        if let Ok(info) = ollama.inspect(&model) {
            engine.set_fenced(!info.supports("tools"));
            let kv = oxigolem::provider::ollama::kv_bytes_per_token(&info.raw);
            let num_ctx = oxigolem::provider::ollama::resolve_num_ctx(
                info.context_length,
                oxigolem::provider::ollama::avail_ram_bytes(),
                kv,
            );
            engine.set_options(ChatOptions {
                num_ctx,
                think: info.think_for(&model),
            });
        }
    }

    // Ten turns: mix plain chat and bash calls so tool plumbing
    // shows up in the numbers.
    let prompts = [
        "Reply with exactly the word: ready",
        "List the files in the current directory using the bash tool, then say just their count.",
        "What is 7 * 6? Answer with one number.",
        "Run `pwd` with the bash tool and tell me the directory name only.",
        "Say the alphabet's first 5 letters.",
        "Run `echo hello-oxi` with the bash tool and repeat its output.",
        "Name one planet. One word.",
        "Run `uname -s` with the bash tool and tell me the OS.",
        "What is the capital of Japan? One word.",
        "Summarize this session so far in one short sentence.",
    ];

    println!();
    println!("model: {model}");
    println!(
        "{:>4} {:>7} {:>7} {:>7} {:>8} {:>8} {:>8}",
        "turn", "prompt", "cached", "hit%", "prefill", "t/s", "load"
    );
    let mut sum_cached = 0f64;
    let mut sum_tps = 0f64;
    let mut n = 0usize;
    for (i, p) in prompts.iter().enumerate() {
        let u = run_turn(&mut engine, p).unwrap_or(Usage {
            prompt: 0,
            cached: 0,
            eval: 0,
            prefill_ms: 0,
            eval_ms: 0,
            load_ms: 0,
        });
        println!(
            "{:>4} {:>7} {:>7} {:>6.1}% {:>6}ms {:>7.1} {:>6}ms",
            i + 1,
            u.prompt,
            u.cached,
            u.cached_pct(),
            u.prefill_ms,
            u.tokens_per_sec(),
            u.load_ms,
        );
        sum_cached += u.cached_pct();
        sum_tps += u.tokens_per_sec();
        n += 1;
    }
    println!(
        "── avg: {:.1}% cached · {:.1} t/s",
        sum_cached / n as f64,
        sum_tps / n as f64
    );
}
