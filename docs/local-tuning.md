# Local-model tuning for oxigolem

How oxigolem keeps a small Ollama model fast on a laptop: what the
app does automatically, what the user controls, and the knobs that
matter when a reply is slow.

## The big win: KV prefix caching

Ollama caches the KV state of every prompt prefix it has evaluated.
When the next request starts with the exact same bytes, prefill is
~200× faster (measured on qwen3:1.7b, 2.4k-token prompt: 20.3 s cold
→ 0.09 s cached).

oxigolem's request format is append-only by design:

- The system prompt is frozen at startup (never rebuilt per turn).
- History is replayed verbatim — tool results and assistant messages
  are appended, never rewritten.
- Compaction (`budget.rs`) picks a cut boundary once and commits to
  it. The boundary never moves — a moving boundary would rewrite
  bytes mid-request and invalidate the whole cache.

Watch the hit rate in the footer: `· 99.4% cached · 41 t/s`. A drop
to near 0% means something rewrote the prompt — check `/doctor` to
confirm `num_ctx` and model are unchanged.

## Per-request options (Phase 1)

Every chat request carries:

```json
"options": { "num_ctx": N, "num_predict": 2048,
             "temperature": 0.1, "top_p": 0.9 },
"keep_alive": "30m",
"think": true|false
```

- `num_ctx` is resolved once at connect time: model's
  `context_length` from `/api/show`, capped by a RAM budget
  (KV bytes/token × num_ctx must fit), floored at 8192. Sent on every
  turn so the served context never changes mid-session.
- `keep_alive: "30m"` keeps the weights loaded between turns — no
  reload stall on the next reply.
- `think` defaults **off** even on thinking models — reasoning tokens
  eat the context a small model can't spare. Re-enable per model in
  `.oxigolem/config.json` (`models.<name>.think: true`).

## Model lifecycle

- `/api/show` (inspect) is called once per model, then cached in
  memory and on disk at `.oxigolem/models.json`. A restart doesn't
  re-inspect.
- Model lists (`/api/tags`) fetch on a worker thread so a dead host
  doesn't freeze the picker; the last live list persists to
  `.oxigolem/model-list.json` as an offline fallback.

## Tool calls on small models

- Models with the `tools` capability get native `tool_calls`.
- Models without it run **fenced repair**: the engine scans the reply
  for ```` ```bash ```` blocks or `{"command": "…"}` JSON and runs
  the first plausible call. `/doctor` shows `tools: fenced repair`.
- 2+ read-only bash calls (allowlist: grep/cat/ls/sed/head/tail/wc/
  find/git-read/cargo-read/…) run in parallel — 4 at a time — via
  scoped threads. Anything mutating falls back to sequential.
- A foreground call that outlives the 120 s block budget isn't
  killed — it demotes to a background shell (Ctrl+B×2 forces this
  early). The model gets the shell id + log path to poll.

## /doctor

Palette → `/doctor` prints into the transcript:

- provider / model / num_ctx / think / tool mode
- last turn: `% cached`, `prefill ms`, `t/s`, `load ms`

Use it to verify a change took effect — e.g. that a config `think`
override actually reached the engine, or that hit rate stays >95%
after several turns.

## When a turn is slow

1. `t/s` low but cached% high → generation is compute-bound; a
   bigger GPU or smaller model, not an app problem.
2. `cached%` near 0 → prefix broke. New system prompt? Compaction
   just fired? Model switched? `/doctor` shows `num_ctx` so a
   different ctx between sessions is visible.
3. `load ms` in the thousands on every turn → `keep_alive` isn't
   reaching the server (pre-0.1.27 Ollama) or the OS is paging the
   weights out. Check RAM.
