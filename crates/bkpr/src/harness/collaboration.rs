//! Native default approvals cover collaboration only; capture and channel checks still apply.

/// Exact Bkpr tools eligible for native default approval in a managed launch.
pub(super) const TOOLS: &[&str] = &[
    "channel_ack",
    "channel_create",
    "channel_history",
    "channel_list",
    "channel_members",
    "channel_post",
    "channel_read",
    "channel_wake",
    "discover_agents",
    "whoami",
];

/// Render each eligible tool as one exact Claude permission rule.
pub(super) fn claude_rules() -> impl Iterator<Item = String> {
    TOOLS.iter().map(|tool| format!("mcp__bkpr__{tool}"))
}
