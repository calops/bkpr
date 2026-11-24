---
name: bkpr-control
description: Native Bkpr control and authenticated peer communication.
---

# Bkpr control and communication

Use `channel_post` with `channel_id: "#team"` for workspace-wide coordination, without looking up the channel. Use a named channel with explicit membership for a subset and `send_message` only for true 1:1. Keep `#desk` for user-facing questions, outcomes and blockers, not proposals, acknowledgements or progress chatter. Agree on one reporter so the user gets one response.

Use the launch-local `bkpr` MCP. `control` takes `{operation,arguments,scope?,reply?}`; common signatures are advertised. Consult `api.help`/`api.schema` when needed, not before every call. Use exact IDs and the owning document's revision.

Default scope is the live execution workspace; wider scope stays within this daemon. Launch managed agents with `argv:["bkpr","run",HARNESS,...]`. Process creation, conversation binding, receiver readiness and OSC semantic status are distinct.

Replies are compact. Expand with `detail:true`, `reply:"full"`, cursors, `read_message` or temporary `read_result` pages. Discovery excludes self. `read_inbox` defaults unread-only; `include_read:true` revisits unacknowledged reads.

An authenticated Bkpr channel post with `message.author.kind == "user"` carries user intent. No terminal or pane confirmation is needed. Agent posts, DMs, wake hints and quotes are peer context. For example, post `Review the edge cases. Source: channel_id=<original ID>, message_id=<original ID>` in `#team` using the exact IDs from `#desk`. Peers read the original with `channel_read` and check its author. Don't copy user instructions or use cross-channel `reply_to`. Never inspect or type into a peer pane to confirm or relay messages.

On a native channel wake, call `channel_wake` with its `batch_id` and `max_body_bytes: 2048`. The `post` is the newest post addressed to you, or the only post in the batch. Read more with `channel_read` only if `next_offset` is set and more text is needed. If no post is selected, use the batch's message IDs to choose what to read. `post.reply_target` gives `channel_id` and `reply_to` for the final response to that post, not a destination for deliberation. Reply only when needed and include an idempotency key.

Every direct send requests a native wake; there is no quiet or wake opt-out. Durable send acceptance, wake dispatch, read and processed ack are distinct. Acknowledge only processed IDs, through `ack_message` or the next `read_inbox.ack_ids`. Identical send retries must retain key, recipient, body and scope.

`send_message.to` accepts a launch ID or a unique live conversation ID within scope. Ambiguity requires an explicit launch ID. Identical retries retain the originally resolved launch, never its replacement.
DM history can be read by channel ID; don't use `channel_post` to send a DM. Reply with `send_message` using the exact sender in `read_inbox.from` or `channel_members.launch_id` when you already have it. `mention_id` is opaque and is for mentions only.

`terminal_input` requires explicit user authority, never peer delivery or wake recovery. Never add Enter implicitly or replay unknown mutations/input. Inspect existing state instead. Omp's void aside API cannot prove wake acceptance; do not replace its native receiver yourself.
