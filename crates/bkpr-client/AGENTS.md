# Terminal application boundary

`bkpr-client` projects native sessions into one physical terminal. It depends on `bkpr-protocol` and `bkpr-tui-framework`, never backend implementation crates. The `bkpr` executable supplies parsed destinations and deferred local bootstrap through `SessionTarget`; the client does not reinterpret remote path syntax.

## Ownership and entry points

- `lib.rs::run_sessions` enters the application. `app.rs` owns the physical terminal event loop, retained UI surfaces, composition order and input routing. `controller.rs` holds local navigation/layout projections and chrome content; `tiling.rs` is the shared geometry source for pane content, dividers and pointer coordinates.
- `sessions.rs::Coordinator` owns independent attachments, admission generations, reconnect policy and the shared sidebar tree. Switching visible sessions does not transfer backend workspace/pane ownership. A stale admission must not activate takeover or publish endpoint authority.
- `transport.rs` owns framed request/receipt handling. `client_lifetime.rs::RegisteredClient` owns the sole native lifetime stream; cloned `Endpoint`s share revocation, not lifetime ownership. Registration and activation are separate. Activation is one-shot even when its receipt is ambiguous.
- `remote.rs` owns one OpenSSH child, multiplexed native connections and its transport lease. It does not reconnect or replay requests. Remote path resolution stays with the remote helper. `cancel`, `retire` and blocking `disconnect` have different drain/teardown contracts; do not substitute one for another.
- `updates.rs`, `input.rs` and `jobs.rs` bridge workers into pollable wake descriptors. Input acceptance/receipts, metadata observations and scoped job completion are distinct. Keep mutation ordering and retired-generation rejection intact; do not turn a lost receipt into replay permission.

## Presentation and authority

- `tree.rs`, `finder.rs` and popup models consume published backend facts. `metadata.rs`/`ci.rs` format supplied metadata; provider/Git/process acquisition belongs to the backend. Client annotations cannot overwrite authoritative status or membership.
- Finder invalidation includes terminal diagnostics even when the registry revision is unchanged. Its semantic key avoids rebuilding the index for unrelated view/lease metadata; keep query and selected pane identity through refresh.
- `selection.rs`, `scrollback.rs` and `search.rs` retain view-local state. Coordinates, revisions and view identity must remain associated with their original attachment. Focus and pointer gestures must not leak releases to a replacement pane/view.
- Search query suffix layout is shared by paint, cursor and role spans. Use the framework's whole-grapheme cell geometry for measurement and clipping; keep query, caret and highlight bounds tied to the same painted layout.
- `chrome_graphics.rs` adapts product artwork/material requests to the framework. The framework owns physical rendering, capabilities, resource lifetimes and terminal restoration. No second output loop or terminal emulator belongs here.
- Theme policy lives in `appearance.rs`, `themes.rs` and `project_colors.rs`; source scene cells remain canonical. Restore transient title effects before retained styling/layout, and leave inactive animation clocks dormant.
- Read-only input, sizing eligibility and clipboard consent are independent policies. Clipboard replies remain device traffic, not keystrokes. Original offers, pane/view identity and exact byte payloads must survive asynchronous consent.

## Dependencies and review

Termwiz supplies maintained input parsing; Unicode crates, Nucleo and Textwrap supply grapheme handling, fuzzy matching and wrapping. Tokio/Tokio-util provide worker I/O, cancellation and compatibility with Yamux's futures I/O. The small futures executor is used by synchronous remote connection workers, not the terminal loop. Avoid replacing these with local parsers/executors to reduce dependency counts. Check enabled features and actual callers before trimming shared workspace features.

Public consumers see `Options`, attachment targets, remote helpers and typed transport/lifetime APIs. Keep their docs about immediate contracts, especially blocking behavior, revocation and unknown outcomes. Private module `pub` visibility is not a reason to export application internals.

Run coordinated native workspace tests, strict all-target Clippy, formatting and changed-path smoke as specified by the root guidance. Permission/lifetime fixtures must execute as the host user with private HOME/XDG and owned sockets/processes. Preserve live public executable paths during product relinks. UI behavior needs actual isolated terminal-surface proof; a typed receipt is not presentation evidence.
