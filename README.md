# oxigolem
A lightweight, Rust-based, model-agnostic AI harness built for local and cloud models, with just one tool: the terminal, minimizing token overhead while keeping things simple, efficient, and powerful.

## Local-model performance

Measured against Ollama 0.40.13, `qwen3:1.7b`, 10-turn session
(`tests/local_perf.rs` — run with `cargo test --test local_perf --
--ignored --nocapture`).

Baseline (before hardening): every turn was a cold prefill, model
reloaded on idle, runaway tool calls killed the agent thread.

| turn | prompt | cached | hit% | prefill | t/s | load  |
|-----:|-------:|-------:|-----:|--------:|----:|------:|
|    1 |    342 |      0 |  0.0 | 2678 ms | 37.7 | 3400 ms |
|    2 |    423 |    367 | 86.8 |  612 ms | 33.3 |   1 ms |
|    3 |    447 |    358 | 80.1 |  759 ms | 26.8 |   1 ms |
|    4 |    518 |    471 | 90.9 |  602 ms | 20.9 |   3 ms |
|    5 |    549 |    462 | 84.2 |  759 ms | 24.0 |   3 ms |
|    6 |    620 |    577 | 93.1 |  560 ms | 24.3 |   1 ms |
|    7 |    641 |    568 | 88.6 |  684 ms | 27.0 |   1 ms |
|    8 |    703 |    665 | 94.6 |  535 ms | 38.5 |   1 ms |
|    9 |    724 |    656 | 90.6 |  634 ms | 26.3 |   1 ms |
|   10 |    749 |    711 | 94.9 |  507 ms | 15.7 |   1 ms |

Average: **80.4% cache hit · 27.5 t/s**.

The cold start is a one-time cost; every subsequent turn reuses the
KV prefix (`prompt_eval_cached_count` ≈ 85–95% of prompt tokens) and
`keep_alive: "30m"` keeps the weights in RAM (`load 1 ms`). The
prefix-stability work in `agent/budget.rs` guarantees the request
bytes are append-only, which is what lets the cache hit at all.

Isolated measurement earlier in the work: a 2.4k-token turn went
from 20.3 s cold prefill to 0.09 s cached — a 223× speedup.

See `docs/local-tuning.md` for the knobs: num_ctx resolution, think
defaults, fenced tool repair, parallel read-only batching, and
`/doctor` (palette → `/doctor`) which shows the same numbers in-app.
