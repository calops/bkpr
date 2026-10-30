# Pane-first mux implementation plan

## Direction and branch scope

`feat/pane-first-mux` is a replacement branch, not a compatibility track. Its executable is the native `bkpr` application built from `bkpr-mux`; `bkpr-terminal` supplies the current source-pinned terminal adapter. The previous launcher, Zellij/WASI plugin, runtime input and release packaging are removed. Neither old tests nor feature/API parity with `main` block work on this branch.

`main` and the retained Zellij fork are starting material, not specifications. Inspect relevant current source as a subsystem comes into scope; keep useful code immediately where it forms a coherent module, refactor poor boundaries, and replace designs where better alternatives fit. Do not retain dormant legacy trees merely to preserve them. Record provenance and applicable notices when substantial code is copied. The source ledger below identifies places to look, not mandatory implementations.

The product model remains pane-first: a pane owns a process/PTY and canonical terminal state; layout/session documents reference endpoint-qualified panes independently of their lifetime. Backend workspace membership and agent authority do not follow a viewer's layout selection. TUI, browser and mobile clients may project the same state differently, including one selected pane without sidebar chrome.

The [initial experiment report](engine-proof.md) records historical evidence at `e9ee402`. It is not production acceptance. The native compositor, initial-presentation and startup-input observations still require closure; deleting Zellij is not permission to label them passing. Backend, metadata and agent work need not stop while independent host/rendering investigations proceed.

## Agreed Cargo workspace

This is the agreed target structure, not the current on-disk layout. Currently `bkpr-mux` combines the executable, backend and terminal client, while `bkpr-terminal` contains the engine adapter. Split these along the boundaries below; do not introduce compatibility aliases or empty crates merely to match the diagram.

| Package/crate | Kind | Executable | Responsibility |
| --- | --- | --- | --- |
| `bkpr` | Executable package | `bkpr` | User entrypoint, frontend selection/bootstrap and utilities such as harness launch commands and explicit integration installation. Opens the terminal client by default. |
| `bkpr-client` | Library | — | Bkpr-specific terminal application: workspace/sidebar/popup content, navigation, interactions and daemon communication. |
| `bkpr-tui-framework` | Library | — | Reusable drawing/composition framework: terminal scene surfaces and widgets, layout, decorations, clipping, layers, hit testing, damage, graphics-resource lifetime and coordinated terminal output. |
| `bkpr-server` | Executable package | `bkprd` | Daemon startup, configuration and orchestration of backend services. |
| `bkpr-core` | Library | — | Backend pane/process lifecycle, multiplexing, workspace/layout registry, harnesses, agent communication, Git/CI and project metadata, persistence/recovery and authority. |
| `bkpr-protocol` | Library | — | Shared identities, requests/responses, events, capabilities, errors and terminal scene types. No frontend or backend implementation dependencies. |
| `bkpr-terminal-engine` | Library | — | Backend terminal-emulation adapter: ingest application output, extract canonical state, encode input and expose terminal effects. Adopt compatible upstream Ghostty Rust bindings; do not maintain a parallel generic SDK. |

Separate crates do not imply separate processes. `bkpr` calls the terminal client library in-process; `bkprd` runs separately. Bare `bkpr` should connect to or start the local daemon and open the TUI. Management commands remain usable without initializing terminal drawing or requiring a TTY. Closing the client does not terminate backend-owned panes. Remote hosts run their own server with the same endpoint API.

The client consumes `bkpr-protocol`, not backend implementation internals. The server assembles `bkpr-core`; the core uses the terminal-engine adapter. Keep backend business logic and authoritative associated data out of the frontend. Client-local focus, viewport, device capabilities and consent presentation remain frontend concerns. Explicit integration installation acts on the machine/account where the command runs, not silently on a remote endpoint.

Application UI is described once through `bkpr-tui-framework`. The framework seamlessly selects rich or plain presentation from observed capabilities and user preferences; application components do not maintain separate fancy/non-fancy implementations. Decorative graphics can fall back to characters, but meaningful unsupported content needs an honest indication. Pane scenes, sidebar, popups and decorations share geometry, clipping, layer order, hit testing, cursor policy, damage and one coordinated terminal writer. The framework presents canonical scenes; it is not another terminal emulator.

Evaluate Ratatui or another maintained foundation underneath that framework. If integration creates competing render loops or conflicting text/image ownership, fix the integration or choose a lower-level foundation rather than leak the conflict into application components.

Add a GUI client only when implemented; `bkpr` may select it later without changing backend authority. Harness adapters, Git/CI, recovery and storage initially remain modules in the relevant libraries. Extract additional crates only for concrete shared consumers, independent interfaces or dependency boundaries.


## Reuse and design decisions

Prefer maintained libraries for terminal state/input encoding, PTYs, codecs, networking/authentication, widgets and storage. Own Bkpr's agent/harness behavior, lifecycle, control, metadata and permission policies. Copy a small useful algorithm when that is simpler than importing an application; do not add another terminal parser or mux authority beneath the canonical engine.

For each substantial dependency or source port, record:

- The concrete capability and smallest useful public boundary.
- Source/version, maintenance and upgrade ownership, supported platforms and reproducible acquisition.
- License/notice obligations, unsafe/FFI exposure and untrusted-input/resource bounds.
- The measured behavior and failure cases that justify adoption, rather than an unchanged legacy implementation test.

The current libghostty-vt C API is unstable. Community [`libghostty-rs`](https://github.com/Uzaaft/libghostty-rs) supplies safe/sys crates; its checked Git API already wraps clipboard reads/writes, OSC 7501, render holds and resolved Kitty images. Prefer a compatible pinned upstream Rust/native pair before production adoption. The initial adapter's generated bindings are temporary, not a commitment to a parallel SDK. Validated paste-offer provenance remains a concrete narrow upstream contribution; don't mix Rust bindings from one native ABI with an arbitrary newer/patched archive.

Use `portable-pty` for OS PTY mechanics. The current `termwiz`, Crossterm and codec dependencies are infrastructure choices, not authority over product design. Evaluate a widget library for chrome only when needed. Use system OpenSSH over stdio without a transport PTY for remote endpoints; reuse host verification and selected authentication rather than inventing SSH or changing user credentials. Browser/TLS/authentication and transactional storage dependencies are chosen when their concrete contracts are implemented.

## Starting-material ledger

These revisions were inspected when removing the old runtime. Recheck current relevant `main` when porting; the listed revisions preserve provenance even if its implementation later changes.

### Bkpr source

Repository `calops/bkpr`, inspected main `9ad93bd809f8fb0dbd0c28bcbc267a682a21430b`, under `crates/bkpr/src/`:

| Area | Useful starting material | Redesign boundary |
| --- | --- | --- |
| Process recognition | `agent.rs`, `agent_contract.rs` | Separate process/CWD observations from authenticated pane/launch authority and semantic status. |
| Harness launch/binding | `harness.rs`, `claude.rs`, `codex.rs`, `codex_startup.rs`, `omp.rs`, `agy.rs`, `agy_receiver.rs`, `install.rs`, `installed.rs`; `integration/omp/index.ts` | Keep genuine harness capabilities and exact conversation/session binding; replace native-owner and plugin rendezvous dependencies. |
| Durable agent communication | `relay.rs`, `relay_tests.rs` | Reuse useful durable receipt/delivery logic; redesign ownership, storage or scheduling where warranted. Durable acceptance, wake, read and processed acknowledgement remain distinct product facts. |
| SSH agent forwarding | `agent_relay.rs` | Lease-selected upstream, raw path bytes and generation cancellation are useful; bind policy to the new execution/client model rather than old launcher locks. |
| Git/CI metadata | `project.rs`, `project_cache.rs`, `project_watch.rs`, `ci.rs`, `ci/github.rs`, `ci_contract.rs` | Extract backend services from Zellij pipe/session publication. Preserve authoritative repository/ref/account facts, not worker layout. |
| Recovery/layout reasoning | `recovery.rs`, `recovery_contract.rs` | Claim/liveness and split-tree algorithms may help; replace KDL/sidebar/session-owned process assumptions. New registry formats need not preserve old wire schemas. |
| Typed control/context | `context.rs`, `context_contract.rs`, `control.rs`, `control/{arguments,operations,native}.rs` | Reuse useful intent/schema ideas; design endpoint/pane capabilities and new identities rather than emulate old native operations. |

No disconnected copies of these domains are retained just to make them compile. When a domain is implemented, select useful code and tests against the new requirements; discarded mechanisms do not create regression obligations.

### Native fork source

Repository `calops/bkpr-zellij`, inspected main `d7b40a37a63d4934054516f6f445488348075a62`. MIT license, copyright 2020 Zellij contributors; preserve the notice for substantial copies. The sibling checkout contains user work and is read-only for this replacement task.

Rendering, image clipping/damage, cursor/focus, client surfaces, terminal capability/palette handling, input demultiplexing and clipboard transport are explicit reuse candidates. Do not import the fork's `Screen`, tab/plugin ownership or second terminal grid as a shortcut. Capture exact paths, symbols and dependencies when selecting a port; the fork is not a maintained SDK boundary or a specification the new client must reproduce.

| Candidate | Starting source | Fit for the new client |
| --- | --- | --- |
| Host capability/palette model | `zellij-server/src/terminal_colors.rs`, `TerminalColors::update` | Small fixed-size observed-client state. Replace Zellij IPC/event adapters and its `xparse_color`/`AnsiCode` dependency with the new client boundary. RGB color replies don't establish truecolor capability; palette reset shouldn't erase independent capabilities. Current native `ColorProbe` already covers overlapping behavior, so avoid a duplicate module. |
| Damage and wide glyphs | `output/mod.rs`, `OutputBuffer::changed_chunks_in_viewport`, `changed_rects_in_viewport` | Adapt the algorithms to owned Ghostty frames: coalesce damage, retain row backgrounds and complete wide/combining glyphs. Don't copy the old grid or `TerminalCharacter` model. |
| Layer/cursor/pointer visibility | `output/mod.rs`, `FloatingPanesStack`; `output/client_scope.rs`; `panes/floating_panes/client_scope.rs` | Select client-visible ordered layers before text/image occlusion, cursor visibility and hit testing. Equal rectangles don't imply equal layer identities. Replace plugin-configuration ownership with explicit typed client/view state. |
| Host image resources | `output/mod.rs`, `HostKittyState`, `serialize_kitty_frame`; `panes/kitty_graphics/store.rs` | Useful transmitted-resource versus placement lifetime, explicit host-clear invalidation, quota/refcount/cache ideas. Adapt where useful; don't copy grid-side APC parsing or create a second canonical image model. |
| ANSI style transitions | `panes/terminal_character.rs`, `CharacterStyles::update_and_return_diff`; `output/unit/output_tests.rs` | SGR 22 clears both bold and dim: emit the desired pair when resetting either. Preserve the observable property, not the old serializer/cell implementation. |
| Clipboard framing and origin | `zellij-client/src/stdin_ansi_parser.rs`, `ClipboardForwardSlot`, `prepare_clipboard_forward`, reply/paste collectors; `zellij-server/src/screen.rs`, `ClipboardPasteGrant` | Fresh host request IDs prevent late/reused-ID confusion; preserve packet boundaries, exact MIME and original pane/client/selection/offer. Rewrite scheduling and grants against the native endpoint model, not `Screen` maps or plugin events. |
| Host query/input routing | `zellij-server/src/host_query.rs`; `zellij-client/src/lib.rs` query dispatch and `stdin_ansi_parser.rs` reply classification | Useful bounded query/reply isolation and capability evidence. Use maintained parsing/encoding primitives, keep keyboard bytes distinct, and don't inherit old shared-focus/input architecture. |

Fork behavior tests are useful evidence and bug examples, not an immutable suite or a requirement to duplicate old types, IDs, byte order or scheduling. Rasterized recessed/raised frames remain optional source material for future chrome rather than an immediate renderer dependency.

## Contracts to make concrete

| Boundary | Responsibilities |
| --- | --- |
| Pane runtime | Real spawn/PTY/engine, process birth identity, resize, bounded I/O, snapshots/events, exit and explicit terminate. Views do not own process lifetime. |
| Canonical scene | Owned typed text/styles/widths, cursor, modes, colors, links, images/placements, damage and resource generations. A client doesn't parse a replay of the PTY into a competing terminal state. |
| Registry/layout | Durable endpoint/pane/workspace/layout identities and revisioned intent. Layout removal, disconnect and process termination are different operations. |
| View authority | Independent eligibility/capabilities for sizing, input, clipboard and control. Latest eligible visible attachment owns size unless pinned; input-read-only does not itself imply a sizing decision. |
| Control/metadata | One typed dispatcher for CLI/MCP, exact qualified targets, backend facts and namespaced frontend annotations. Creation doesn't imply focus, approval or readiness. |
| Endpoint transport | Bounded versioned framing, authentication, capability checks, cancellation, reconnect and resnapshot. SSH authentication alone doesn't authorize every pane operation. |
| Client projection | Local chrome/layout selection, actual terminal rendering/input/device replies, selection/scrollback, accessible interaction and platform-specific clipboard. |
| Agent services | Authenticated launch/conversation binding, truthful readiness, durable delivery/acknowledgement and appropriate native steering. Never type peer notifications into PTYs. |

Define identity epochs, revision ordering, subscriptions/coalescing, resnapshot, slow-client limits, permission expiry/revocation and error semantics before implementing concurrent consumers. A socket write or painted status badge is not acknowledgement of process input or model work.

## Delivery sequence

### 1. Native branch baseline

Remove old runtime and build/release interfaces; make native `bkpr` the only executable path. Keep real engine/PTY/view/device behavior, relevant behavioral tests and historical evidence. Update README, project guidance and backlog without pretending unsupported old features still exist. Use only native checks; protect unrelated running programs and shared public artifact paths as an operational precaution, not a compatibility requirement.

### 2. Engine/client correctness and dependency adoption

Close the actual initial-image and startup-input observations with causal evidence, not periodic redraw, sleeps or false capability advertising. Exercise alternate screen, Unicode/width, reflow/scrollback, cursor, modes, key/mouse/focus, paste, links, colors and resource lifetime through real consumers. Keep exact MIME/selection/offer grants and query/reply isolation. Test allow/deny/stale/timeout/disconnect and unrelated-pane progress during clipboard consent.

Use source from the fork where it improves damage, image projection or device handling; prefer upstream libraries where their boundary fits. Replace redundant generic bindings through a compatible upstream pair. Sixel is currently unsupported: it is a product support decision before declaring terminal compatibility, not an obstacle to implementing the pane-first backend. Don't write a terminal engine to hide an adoption failure.

### 3. Multi-pane registry and layout documents

Implement durable independently owned panes, explicit termination, backend workspace membership and revisioned layouts with endpoint-qualified references. Demonstrate the same pane in multiple views, removal without termination, detach/reattach, deterministic size pins/leases and independent input/clipboard authority. Document daemon-crash and orphan policies honestly. Bound queues, memory, resources and stale generations; one slow view or synchronous engine callback must not stop unrelated panes.

Persist layout and metadata intent atomically. Keep desired attachment distinct from transient disconnect. Terminal history persistence is a separate opt-in product feature, not an accidental recovery record containing secrets.

### 4. Control, metadata and agent services

Inspect current Bkpr main before working on each domain. Reuse or rewrite its process recognition, Git/provider caching, broker, harness adapters and recovery against the new model. No old command/schema/storage layout is mandatory.

Backend facts retain provenance: OSC 7501 semantic status, independent title/CWD/process observations, canonical worktree identity, actual checked remote/PR commit, selected provider account and explicit loading/error state. Frontend annotations cannot overwrite them. No Git fetch or provider call per redraw.

Agent identities must be authenticated, launch-scoped and bound to genuine native harness conversation IDs. Registration is not readiness. Distinguish accepted delivery, wake, read and processed ack; guard asynchronous outcomes by generation and fail closed on durable storage failure. Prefer harness-native active-turn steering, use future-turn delivery where necessary, never PTY typing or blind ambiguous replay. Recovery creates fresh authority and doesn't adopt an exited launch's inbox. Explicit installations preserve unrelated user settings.

### 5. Full TUI and independent projection

Implement frontend-owned navigation/chrome, splits, finder/palette, metadata, forms, overlays, selection and links against the same scene/control model. Redesign input ergonomics rather than recreate Zellij modes by default. Cursor, pointer and focus ownership are client-specific.

Build a second independently running projection early. A browser or narrow single-pane consumer must render a live terminal from the canonical protocol, handle resource/delta lifetime and reconnect, and coexist with an active TUI. Screenshots, hard-coded mocks or ANSI replay don't establish independent frontend support.

### 6. Authenticated remote endpoints

Run the endpoint API locally and over actual SSH stdio without a transport PTY. Define trust/enrollment, endpoint reinstall/identity rotation, scoped permissions and disconnect semantics. Show local and remote panes in one layout while execution/metadata stay on their owning endpoints. Test binary images/input/clipboard, latency, flow control, drops, stale grants, ambiguous mutations and resnapshot; reconnect must not blindly replay input. Viewing a remote pane doesn't merge agent discovery or credential trust domains.

### 7. Supported products and packaging

Declare the supported OS/terminal/client matrix from evidence. Browser/mobile need real rendering, font metrics, IME/soft keyboard, touch, accessibility, platform clipboard and authenticated connection handling—not just WebSockets. Background mobile views must not silently resize desktop panes.

Restore release/package outputs only with an offline reproducible native engine/toolchain dependency closure, licensing/notices, provenance and upgrade checks. Current Cargo/devshell operation must not be disguised as complete Nix packaging. No old-server import or live-PTY transfer is required; any useful declarative import is a separately designed feature.

## Verification

Run coordinated checks after related slices land: native workspace tests, all-target strict Clippy, formatting and an actual changed-path smoke. Only replacement-relevant tests count. Preserve behavioral boundaries, not source-text, wording, copies or incidental defaults. Do not run deleted Zellij/WASI checks to prevent regressions.

Use private HOME/XDG and sockets, owned process birth identities, fake clocks where useful, and actual maintained harness TUIs against isolated fake local providers. Never call real providers, change user profiles, auto-grant harness approvals or operate on live sessions for fixtures. Keep Linux process helpers/tests platform-gated. Retain useful versions, frames, protocol bytes, physical screenshots and receipts under shared `target/smoke/`, with exact cleanup.

Acceptance is tied to implemented capability: process/view independence, permission and generation boundaries, terminal/device fidelity, resource limits, authoritative metadata, genuine agent binding/delivery, recovery and remote trust. `main` and the fork supply evidence and starting code; they do not dictate the new design or an unchanged regression suite.
