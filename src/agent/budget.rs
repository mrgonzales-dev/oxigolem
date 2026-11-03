//! History budgeting: decide which messages from history to send each
//! turn without ever reshaping the prefix. We compact exactly once —
//! the first time history exceeds the budget we cut a boundary, and
//! that boundary is frozen forever after. A frozen cut keeps the sent
//! request append-only (`[system, history[cut..]]`); recomputing it
//! would rewrite bytes mid-stream and kill the KV prefix cache.

use super::chat::Message;

/// Cheap token estimator — ~4 chars/token is close enough for
/// budgeting; we only need to know whether we're over the cap.
pub fn estimate_tokens(m: &Message) -> usize {
    let mut n = m.content.len() / 4 + 4;
    if let Some(calls) = &m.tool_calls {
        for c in calls {
            n += c.name.len() / 4 + c.arguments.to_string().len() / 4 + 4;
        }
    }
    n
}

/// What to do with history this turn.
#[derive(Debug, PartialEq)]
pub enum Plan {
    /// Fits — send everything.
    KeepAll,
    /// Send `history[keep_from..]`. `keep_from` is an index into
    /// `history` chosen once and reused verbatim on every later call.
    Compact { keep_from: usize },
}

/// Pick a plan.
///
/// `stored` is the cut the caller committed to on a previous call —
/// pass `None` the first time. When `stored` is `Some`, it always
/// wins: the prefix was already dropped and can never come back, so
/// re-evaluating the budget buys nothing and only risks moving the
/// boundary.
///
/// Fresh cut algorithm: accumulate the tail backwards until the kept
/// span would exceed `budget_tokens`, then walk the cut forward to the
/// next `role: "user"` boundary so the kept tail never opens mid-turn.
pub fn plan(history: &[Message], budget_tokens: usize, stored: Option<usize>) -> Plan {
    plan_with_hint(history, budget_tokens, stored, 0)
}

/// `plan` with a server-side prompt count hint. The hint is Ollama's
/// `prompt_eval_count` from the previous turn — when it exceeds the
/// local estimate we trust the server, since it counts what actually
/// filled the context window (system prompt, tool schemas, JSON
/// escapes) and the local chars/4 figure drifts low on tool-heavy
/// sessions. The hint lets compaction fire *before* the request that
/// would have overflowed, instead of after the error.
pub fn plan_with_hint(
    history: &[Message],
    budget_tokens: usize,
    stored: Option<usize>,
    last_prompt_tokens: usize,
) -> Plan {
    if let Some(cut) = stored {
        return Plan::Compact {
            keep_from: cut.min(history.len()),
        };
    }
    let est: usize = history.iter().map(estimate_tokens).sum();
    // The server's count includes the system prompt + tool schema
    // overhead the local estimate can't see — a ~400-token fixed cost
    // we subtract before comparing so history-only estimates stay on
    // the same scale. The hint only tips the KeepAll/Compact decision;
    // the cut itself still uses estimate_tokens per message.
    const FIXED_OVERHEAD: usize = 400;
    let server_history = last_prompt_tokens.saturating_sub(FIXED_OVERHEAD);
    let total = est.max(server_history);
    if total <= budget_tokens {
        return Plan::KeepAll;
    }
    let mut kept = 0usize;
    let mut cut = history.len();
    for (i, m) in history.iter().enumerate().rev() {
        let cost = estimate_tokens(m);
        if kept + cost > budget_tokens {
            break;
        }
        kept += cost;
        cut = i;
    }
    while cut < history.len() && history[cut].role != "user" {
        cut += 1;
    }
    Plan::Compact { keep_from: cut }
}
