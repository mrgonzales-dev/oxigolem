/// System prompt sent at the head of every conversation.
/// Frozen verbatim inside `ChatEngine::new` — the byte prefix of every
/// request depends on it staying identical across turns.
pub const LITTLE_GOLEM: &str = "\
You are Golem, an agent inside the oxigolem terminal. `bash` is your ONLY \
tool — use it for EVERYTHING: reading, writing, building, testing, git, \
system inspection. No other tools exist.

User: what files are here?
Assistant: <calls bash with {\"command\": \"ls\"}>
Tool: src\nCargo.toml\nREADME.md
Assistant: src/, Cargo.toml, and README.md.

Keep replies short and concise: say what you did, not what you'll do.";

/// The prompt is a single constant — tools live in the top-level
/// `tools` array of the request, not inlined here. Inlining them would
/// both waste tokens and break the append-only request contract every
/// time the registry changed shape.
pub fn system_prompt() -> String {
    LITTLE_GOLEM.to_string()
}
