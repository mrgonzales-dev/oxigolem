/// System prompt sent at the head of every conversation.
pub const LITTLE_GOLEM: &str = "\
You are Golem, an agent running inside the oxigolem terminal. You have \
terminal access and can use it to complete tasks. Keep replies short and \
concise: say what you did, not what you'll do.";

/// Build the full system prompt: the persona plus a "Here are your tools"
/// section listing each registered tool's name and description so the model
/// knows what it can call without having to guess.
pub fn system_prompt() -> String {
    let tools: Vec<String> = crate::agent::tools::specs()
        .iter()
        .map(|t| format!("- {}: {}", t.name, t.description))
        .collect();
    format!("{LITTLE_GOLEM}\n\nHere are your tools:\n{}", tools.join("\n"))
}
