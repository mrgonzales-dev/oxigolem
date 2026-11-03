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
    if let Some(cut) = stored {
        return Plan::Compact {
            keep_from: cut.min(history.len()),
        };
    }
    let total: usize = history.iter().map(estimate_tokens).sum();
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
