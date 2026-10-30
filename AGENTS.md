# Engineering guidelines

## Product direction and source reuse

This branch replaces the previous Bkpr implementation. It isn't a compatibility track. Don't maintain a Zellij/WASI backend, preserve obsolete interfaces or keep old tests passing for their own sake.

`main` and the retained `calops/bkpr-zellij` fork are starting material, not specifications. Inspect their relevant current code when a subsystem comes into scope. Reuse coherent useful pieces, refactor weak boundaries and replace designs where better ones fit. Keeping useful code immediately is fine; retaining disconnected legacy trees isn't. Record source revision/path and preserve applicable license notices when copying substantial code, including Zellij's MIT notice.

Use maintained dependencies for terminal emulation, PTYs, codecs, networking/authentication, storage and UI infrastructure. Own Bkpr's agent/harness behavior, lifecycle, metadata and authority policies. Don't maintain a parallel generic SDK when an upstream library fits. The engine adapter uses the pinned `libghostty-rs` safe/sys pair in `vendor/libghostty-rs`; `UPSTREAM.toml` records its exact native ABI and separable dependency patch. Contribute narrow reusable gaps upstream where practical. Updates must reproduce the dependency patch and pass SDK plus product acceptance; never mix arbitrary system Ghostty with the checked Rust declarations.

The implementation plan and source ledger are in `docs/pane-first-mux/implementation-plan.md`. Historical experiments record evidence, not production acceptance. Distinguish implemented behavior, agreed design and unresolved decisions.

## Architecture and crate boundaries

The backend owns independent panes, their processes/PTYs, canonical terminal state and associated authoritative data. Layout/session documents reference endpoint-qualified panes; displaying a pane doesn't own its lifetime or change its backend workspace membership. TUI, GUI, browser and mobile clients may project the same state differently. A mobile view may show one pane without sidebar chrome.

The workspace uses these seven packages. Keep implementation boundaries real, without empty crates, compatibility aliases or old combined prototype trees. Some responsibilities describe future domains, not features already exposed by the current executables.

| Package/crate | Kind | Responsibility |
| --- | --- | --- |
| `bkpr` | Executable | User entrypoint, utilities, connection/bootstrap and frontend selection. Opens the terminal client by default. |
| `bkpr-client` | Library | Bkpr-specific terminal application: navigation, workspace/sidebar/popup content, interactions and daemon communication. |
| `bkpr-tui-framework` | Library | Reusable terminal drawing/composition framework, including rich/plain presentation and terminal scene surfaces. |
| `bkpr-server` | Executable package | Produces `bkprd`; daemon startup, configuration and backend service orchestration. |
| `bkpr-core` | Library | Pane lifecycle, multiplexing, workspaces/layouts, harnesses, agent communication, Git/CI/project metadata, persistence/recovery and authority. |
| `bkpr-protocol` | Library | Shared identities, requests/responses, events, capabilities, errors and canonical scene types. No frontend or backend implementation dependencies. |
| `bkpr-terminal-engine` | Library | Backend terminal-emulation adapter: ingest application output, extract canonical state, encode input and expose terminal effects. Not a frontend renderer or PTY manager. |

- Separate crates don't require separate processes. `bkpr` calls its client library in-process; `bkprd` runs separately. Utility commands don't initialize drawing or require a TTY. Starting the local daemon on demand doesn't make the client its lifetime owner.
- Clients consume the protocol, not backend implementation internals. Keep workspace, harness, broker and metadata services in the backend. Focus, viewport, terminal capabilities and consent presentation are client-local concerns. Frontend annotations can't overwrite backend facts.
- Remote hosts run their own server. Execution and authoritative data stay on the owning endpoint. Authentication, enrollment, permissions, framing and reconnect behavior are explicit contracts, not consequences of opening an SSH connection.
- Harnesses, Git/CI, recovery and storage start as modules in the appropriate library. Extract more crates only for concrete shared consumers, independent interfaces or dependency boundaries. Add a GUI client when implemented, not as a placeholder.

## Responsiveness is a product requirement

Performance is part of every design and implementation decision, not a cleanup phase. Startup, first input, typing, navigation, popup opening, resizing, pane switching and reconnect must feel snappy. Check tail latency and behavior under load, not only average throughput or an idle demo.

Optimize equivalent behavior autonomously. Any proposed performance compromise that changes supported features, visual quality, metadata freshness, consistency, durability, input behavior or materially changes resource requirements must be discussed with the user before implementation. Present the measured problem, options, expected benefit and costs, and a recommendation. Record the agreed decision. Don't silently remove features, lower fidelity, throttle interaction, serve stale data as fresh or defer required work merely to make a benchmark look faster. Already agreed capability-based decorative fallbacks aren't permission to disable supported features for speed.

### Critical paths and scheduling

- Don't put filesystem scans, Git/provider work, process inspection, network calls, image decoding/rasterization or storage I/O on the frontend input/render path or a global backend lock. Schedule expensive work outside those paths and publish results.
- A slow pane, client, endpoint, provider or clipboard-consent callback must not stall unrelated panes or interaction. Bound work per turn, preserve fairness and keep cancellation responsive. Model synchronous foreign callbacks honestly with pane-local isolation or supported continuations.
- Preserve input order and exact delivery semantics under load. Prioritizing interaction doesn't authorize reordering PTY bytes, dropping control events or replaying ambiguous mutations. Apply explicit backpressure and report rejection/acceptance truthfully.
- Prefer push events, invalidation and subscriptions over recurring polling. Coalesce redundant refresh/repaint work without losing lifecycle events, grants or receipts. Timers need a concrete deadline/refresh purpose; don't add sleep-based readiness, busy waits, periodic forced redraws or speculative prefetch to hide a defect.
- Keep startup dependencies minimal. Unrelated endpoints, cold provider caches and decorative assets must not serialize basic local interaction. Show honest loading/offline states. Don't claim input readiness before input works.

### Allocation, rendering and transport

- Avoid needless allocation, copying, serialization and repeated computation, especially per key, cell, frame or subscriber. Borrow or share immutable data where it fits; don't clone full scenes/scrollback or expand image formats for convenience. Measure before adding a complex optimization.
- Cache expensive stable work at the right ownership boundary. Define keys, generation/invalidation, bounded memory and eviction. Don't recompute layout, styles, glyph/image resources or project metadata on unchanged input. Don't retain unbounded caches, tasks, queues, histories or disconnected-client resources.
- Plan composition and damage before output. Repaint changed regions, preserve complete wide/combining glyphs and update image placements/resources without unnecessary retransmission. No timer-driven full-screen repaint for static content. Use a full snapshot/redraw when actually required by initialization, recovery or invalidation, not as the default cost of every event.
- Carry resource identities/generations and meaningful revisions through the protocol. Avoid resending unchanged frames or image bytes to every client. Bound parsing, decompression, encoding and queue costs before allocating; sample only visible image regions where possible.
- Treat bytes and round trips as costs, especially over SSH. Batch related updates and terminal writes without delaying interactive work or violating protocol boundaries. Don't build multiple parsers, render loops or competing scene caches to work around an unclear interface.
- Idle work should be quiet. Stop hidden decorative animation and unnecessary refresh work; an idle application shouldn't burn CPU or wake all panes. Reuse the agreed visibility and scheduling model rather than inventing a second one for each feature.

### Performance evidence

For a change that can affect responsiveness or resource use, exercise the actual path with a reproducible workload. Include cold/warm startup, sustained output, full-screen editor redraws with syntax/styles, many panes/views, large scrollback/images, resize bursts, slow consumers and remote latency as relevant. Sparse typing doesn't establish full-screen responsiveness. Compare repeated navigation and bursts at representative large dimensions, not only one settled frame. Don't require the full matrix for unrelated edits.

Choose measurements that match the user-visible concern: input-to-presentation latency, time to usable UI, frame time/damage, CPU, allocation count, memory growth/peak, idle wakeups, transport bytes or round trips. Distinguish queue acceptance, parsing, frame submission and actual presentation. A protocol ACK isn't proof that pixels appeared.

Physical readiness must cover the expected complete content. A stable black frame, first glyph, submitted frame or cached screenshot isn't a valid reference. Acquire immutable full-frame references after content readiness, and timestamp presentation independently of VT replay or expensive controller analysis.

Record build mode, versions, workload, dimensions, environment and raw evidence. Compare equivalent configurations; don't compare a debug baseline to an optimized candidate or hide tail spikes behind averages. Establish budgets from measurements and agree meaningful product targets with the user rather than invent arbitrary guarantees. Keep controlled benchmarks separate from correctness tests; avoid flaky wall-clock assertions in the ordinary suite.

Don't stop at a plausible performance explanation. Profile the bottleneck, change the responsible layer and observe the result. Never weaken authorization, ownership, exact clipboard bytes or truthful durable acknowledgements to meet a latency target. If the desired behavior and performance conflict, discuss it instead of quietly choosing which requirement to break.

## One frontend composition framework

Application components describe the UI once through `bkpr-tui-framework`. The framework handles fancy/plain presentation from observed terminal capabilities and user preferences. Sidebar, panes, popups and decorations share layout, clipping, ordered layers, hit testing, cursor policy, damage and one coordinated output path.

Evaluate Ratatui or another maintained foundation underneath this boundary. Don't leak competing framework/pane/image render loops into application code. The framework presents backend scenes; it doesn't reparse pane output into a second terminal emulator.

Use terminal-default colors and confirmed per-client capabilities, not a fixed theme or guessed RGB support. Decorative graphics may fall back to characters; meaningful unsupported content needs an honest indication. Keep fallback behavior coherent and user-selectable where appropriate, not a separate application implementation. Fancy presentation must not steal input, move the application cursor or add needless redraws when nothing changes.

## Product facts and authority

- Harness semantic status comes from OSC 7501, not titles, output, progress or process presence. Title/CWD/process observations and notification readiness are separate facts.
- Input, sizing, clipboard and control permissions are distinct. Latest eligible visible attachment owns size unless pinned; input-read-only isn't itself a sizing policy. Unknown ownership/context fails closed.
- Clipboard transport belongs to the native backend/client boundary. Preserve exact MIME bytes and framing. Replies never become keyboard input. Grants bind to their original pane, client, selection and offer; expiry, malformed transfers or disconnect never fabricate contents.
- Agent identities are authenticated and launch-scoped. Discovery, registration, conversation binding, subscriber readiness, durable acceptance, wake, read and processed acknowledgement are distinct. Prefer native steering where supported; never type peer delivery into PTYs or blindly replay ambiguous input/wakes.
- Durable storage failure must not produce successful-looking acknowledgements. Recovery doesn't persist tokens or terminal contents or adopt an exited launch's inbox. Terminal history persistence needs its own explicit privacy/storage policy.
- Associate repositories with foreground CWD and canonical common Git identity; worktrees have independent HEADs. Keep actual checked remote/PR commits distinct from local HEAD. Don't fetch merely for UI or query providers per redraw; respect batching, spacing/backoff and the user's active account.
- Normal execution doesn't change profiles, credentials, persistent third-party settings or GitHub accounts. Explicit integration installation preserves unrelated settings and refuses conflicts. Installation acts on the machine/account where invoked, not silently on a remote endpoint.

These are product constraints, not an obligation to copy old APIs, storage formats, schedulers or implementation tests. Improve designs without confusing observations, authority and presentation.

## Development and verification

Use `nix develop`, incremental Cargo artifacts and the native instructions in README. No Zellij/WASI build or deleted implementation's regression suite is required. Run coordinated native checks after related edits: workspace tests, all-target strict Clippy, formatting and an actual changed-path smoke. Documentation-only changes don't require rebuilding the runtime.

Reuse the existing worktree and canonical shared Cargo target. Don't create per-feature caches or discard incremental artifacts. Changes here must not mutate `main`, sibling checkouts, installed programs or running user sessions. Protect live canonical executable paths with a private public-output view sharing caches/locks, or a known-good immutable executable outside build output. This is operational safety, not a compatibility architecture.

Fixtures use isolated HOME/XDG, private sockets, explicit owned process identities and fake local providers. Never call real model/provider services, change user profiles, auto-grant approvals or operate on live sessions for smoke tests. Verify affected input, pointer, resize, focus, cursor, graphics and clipboard behavior on the actual terminal surface. Retain useful evidence under `target/smoke/`; clean up owned processes and temporary files.

Keep procfs, verified Linux process-birth and subreaper helpers/tests Linux-only. Don't weaken production ownership checks for portability. Use private fixture roots, no process-global environment mutation, and nonblocking PTY reads rather than assumptions about portable byte counts.

Tests should catch consumer-visible behavior, boundaries, state transitions and failure cases. Don't pin source text, incidental defaults, copied values or wrapper forwarding. Remove obsolete mechanism tests rather than rewriting the architecture to satisfy them.

## Commits and releases

Use Conventional Commits. Mark actual breaking changes with `!` and a `BREAKING CHANGE:` footer. Don't label maintenance as a feature merely to force a release. Automatic releases stay in `0.x`; changing to `1.0` requires deliberate user approval. Don't publish experimental builds as production-ready or retain obsolete release plumbing as a successful no-op.
