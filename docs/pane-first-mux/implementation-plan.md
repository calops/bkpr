# Pane-first mux implementation plan

## Status and scope

This is an implementation plan, not an implementation or a runtime verification report. The approved direction and source-backed architectural assessment are in [assessment.md](assessment.md). No dependencies, code, build recipes or runtime configuration are changed by this document. Dependency choices below are recommendations to validate, not declarations that their integration already works.

The target is a Bkpr-owned pane-first mux. A pane is a durable backend process/PTY and canonical terminal state. A session/layout is a document that references endpoint-qualified panes and records split intent and weights; it does not own the processes merely because it displays them. Clients project that document independently. A TUI may show a split tree, a mobile client one selected pane, and a web client another projection. One layout may contain panes from several endpoints. Backend execution and workspace membership do not depend on a pane's current display location.

The current Zellij path remains usable while development proceeds. It is not a permanent second backend. There is no live-PTY migration promise, and a successful prototype is not permission to remove an existing observable feature. The old session/tab-owned remote-workspace design is not the implementation target.

## 1. Ownership and dependency adoption

### Borrow the infrastructure; own the product contracts

Bkpr owns pane lifecycle and identities; endpoint and workspace authority; layout documents and views; size/input/clipboard policies; typed control and metadata; harness adapters and broker integration; frontend composition; persistence/recovery semantics; and the glue between these boundaries. It should not become the maintainer of a large borrowed terminal engine.

Reuse maintained libraries for PTYs, terminal parsing/state, asynchronous I/O, serialization, authentication/TLS, terminal widgets, codecs and storage. Do not wrap a library just to claim dependency reuse while reimplementing its function alongside it. Do not introduce a new framework or database where existing code already supplies the required invariant. A small adapter is justified by a concrete boundary or missing integration contract, not by hypothetical backend interchangeability.

Every new dependency decision records:

- The exact capability needed and why existing dependencies cannot supply it; the smallest enabled features and supported platforms.
- Maintenance/release activity, public support policy, API/ABI stability, platform toolchain requirements and the realistic upgrade owner.
- License compatibility, transitive licenses and any notice/source-distribution obligations; security advisories, unsafe/FFI surface, untrusted-input limits and security-update process.
- Version/source pin, reproducible acquisition/build, testable upgrade procedure, packaging impact and supply-chain review. A git pin is not a stable API guarantee.
- A bounded integration experiment and explicit reject criteria. Experimental dependencies may be added to an isolated adoption spike; shipping adoption waits for that gate. The spike must not silently change the current default runtime or its dependency closure.

The deliberate exception to preferring stable published APIs is the current official libghostty-vt C embedding boundary. It offers unusually relevant terminal, rendering, input and protocol capabilities, but its current signatures are unstable and it has no established stable tagged ABI or official Rust SDK. If adopted, source-pin it behind one Bkpr-owned narrow FFI adapter, with lifetime/ownership tests, callback conformance tests, version checks and a documented upgrade procedure. Do not expose Ghostty structs or raw handles across the application or wire protocol. Do not solve instability by taking ownership of a forked terminal engine or a large third-party Rust wrapper. A small, reviewable upstream contribution or maintained codec integration may fill a measured gap; carrying a broad engine fork is a rejection signal.

### Bounded initial shortlist

These are candidates for experiments, not Cargo changes. Confirm their exact pinned versions, licensing and support before adoption.

| Boundary | Initial recommendation | Gate / alternative |
| --- | --- | --- |
| PTY spawn, resize, read/write, wait and termination | [portable-pty](https://docs.rs/portable-pty/latest/portable_pty/), starting from its public 0.9 API | Prove controlling terminal, shell job control, resize, EOF, process-tree cleanup and required Linux/Darwin behavior. It is not a mux or terminal engine. |
| Terminal parser, canonical grid/scrollback, reflow, render state and input encoders | Official [libghostty-vt C API](https://libghostty.tip.ghostty.org/), source-pinned through the narrow adapter | Phase 1 gates include graphics, clipboard callbacks and packaging. [alacritty_terminal](https://docs.rs/alacritty_terminal/latest/alacritty_terminal/) is a bounded alternative investigation if the gate fails, not an assumed image-capable drop-in. Reopen engine choice rather than building a terminal engine. |
| Async work, bounded channels and serialization | Reuse the repository's Tokio, Serde and serde_json | Extend feature sets only for a demonstrated need. Define bounded buffering and protocol compatibility before selecting a binary codec. |
| TUI chrome and terminal connection I/O | Evaluate [Ratatui](https://ratatui.rs/) with [Crossterm](https://docs.rs/crossterm/latest/crossterm/) | These do not render an arbitrary rich terminal scene for free. Prove raw key fidelity, queries/replies, mouse, graphics and selective repaint; a chrome library must not consume application replies or downgrade input. Evaluate a lower-level maintained terminal I/O library only if a named gate fails. |
| Local and SSH transport | Private local IPC plus the system OpenSSH client over stdio, without a transport PTY | Reuse SSH host verification and user-selected authentication. No custom encryption, SSH stack or silent host-key acceptance. Prove binary framing, cancellation and permissions. |
| Browser transport, TLS and authentication | Evaluate Axum/WebSocket plus rustls when browser work begins; reuse maintained identity/OIDC or authenticated pairing components as the selected deployment requires | Do not add a public listener or bespoke authentication protocol for a projection experiment. Authenticate endpoint, user, client and each capability; a WebSocket is not authorization. |
| Durable state and generic codecs | Keep existing durable broker storage initially; consider SQLite through rusqlite for new transactional registry/layout needs only if those needs are demonstrated. Use maintained image/Sixel codecs if required by the chosen engine path | No store-format rewrite bundled into mux bring-up. Prove migrations, atomicity and failure handling. Evaluate exact codec maintenance, licenses, allocation/decompression limits and lossless requirements before selection. |

[Zellij's published server crate](https://docs.rs/zellij-server/0.45.1/zellij_server/), [WezTerm's internal mux crate](https://github.com/wezterm/wezterm/blob/main/mux/Cargo.toml), and [tmux control mode](https://github.com/tmux/tmux/wiki/Control-Mode) do not presently supply the required independent pane/layout embedding contract. Do not add them as another mux authority beneath the new registry. Termwiz and widget libraries may supply narrowly useful rendering/I/O infrastructure; they are not a shortcut to a complete frontend. The assessment records the source research and limits of these conclusions.

## 2. Freeze the contract before parallel implementation

Phase 0 owns the API contract as one coherent task. Only after its review may independent engine, registry, frontend and harness work proceed against it. Do not invent multiple competing pane/frame/identity schemas in parallel and reconcile them after code exists.

The following are proposed module boundaries, not mandated crate names or a permanent `Backend` trait implemented by both Zellij and the new runtime:

| Boundary | Responsibilities and proposed interface |
| --- | --- |
| `pane_runtime` | Own each PTY/process lifetime and one terminal-engine instance. `spawn(SpawnSpec) -> PaneRef`, `terminate(PaneRef, policy)`, `inspect`, `read`, `subscribe(cursor)`, authorized `input` and lease-validated `resize`. Install canonical context before exec; serialize mutations per pane. A view disappearing is not a termination request. |
| `terminal_adapter` | Sole FFI owner; ingest PTY bytes; expose owned/versioned snapshots and deltas, engine input encoding, terminal replies, and typed side-effect callbacks. Return explicit supported capabilities. Keep parser/render memory inside a documented lifetime and translate to Bkpr types without copying entire scrollback per frame. |
| `terminal_protocol` | `PaneRef { endpoint_id, pane_id }`, endpoint/process birth generations, frame epoch/sequence, snapshot/delta base revision, dimensions and pixel metrics; cells/styles, grapheme widths, cursor, palette/default colors, links, modes, image resources/placements and damage. Typed events include exit, title, CWD, OSC 7501, bell, capability and clipboard requests. Input/output and control have distinct message types and limits. |
| `layout_registry` | Persist session/layout documents of qualified pane references and split weights, independent backend workspace membership, view/client projections and frontend-owned namespaced annotations. Mutations use document revisions. Removing a view/reference and terminating a pane are distinct typed operations. Disconnection is observed state, not deletion of desired attachment. |
| `view_authority` | Track authenticated client/view generations, visibility, interactivity, focus and explicit pins. Grant size leases to the latest eligible visible interactive view unless pinned; background/read-only views cannot acquire them. Input and clipboard grants are separate and never implied by the size lease. |
| `control_metadata` | One typed dispatcher for CLI and MCP; inspect authoritative pane/process/context and metadata, mutate exact qualified targets, publish revisioned events. Preserve approval boundaries and expose schemas/capabilities, not native host command strings. Frontends add namespaced annotations with provenance but cannot overwrite status, CWD, lifecycle or repository facts. |
| `endpoint_transport` | Framed authenticated requests/events over local IPC or SSH stdio; browser transport later. Negotiate schema/capability versions, session and client generations; implement cancellation, bounded queues, reconnect snapshot/resubscribe and resource authorization. No terminal ANSI stream pretending to be the structured scene protocol. |
| `frontend_tui` and other frontend adapters | Consume the same metadata/layout/terminal protocol; choose projection and chrome locally; report eligible view state; request input/clipboard grants. Each supplies actual scene rendering, input, IME/accessibility and clipboard adaptation appropriate to its platform. |
| `launch_context`, agent adapters and recovery | Translate existing product contracts onto new pane/workspace authority. Keep broker durable receipt semantics and harness-native binding/notification paths, independent of frontend composition. |

### Required invariants in the frozen contract

1. Endpoint IDs are explicit and collision-safe. Pane identity is not a reusable OS PID, native pane number, tab ID, title or layout slot. Pane owner/process birth, client generation and request correlation protect stale callbacks and responses. Define endpoint enrollment, reinstallation and identity rotation behavior.
2. Layout/session identity, backend workspace membership and frontend projections are separate. A remote pane displayed in a local session remains owned by its remote endpoint. Displaying a pane does not silently change agent discovery scope or promote the viewer to backend owner. Define explicit authorized workspace reassignment rather than inferring it from layout moves.
3. Spawn binds authoritative context before exec. Recovery creates fresh runtime and launch identities. Environment context is an immutable launch snapshot; live context/labels come from the authoritative API. Process recognition and readiness are observations, not authorization.
4. Frames carry enough resolved state for a frontend without a second VT parser: text with width information, cursor and modes, colors, hyperlinks, graphics and damage/resource lifetimes. Neither raw PTY replay nor full rendered ANSI is the client scene model. Define Unicode-version compatibility and selection/scrollback read semantics.
5. Deltas have ordering/base revisions and a bounded resnapshot path. Slow or disconnected clients cannot indefinitely retain engine buffers, image resources or unbounded output queues. A blocked client cannot stop unrelated panes. Define permitted coalescing; lifecycle, grants and broker receipts cannot be dropped as repaint damage.
6. The size lease uses frontend-reported content dimensions after its chrome exclusion. Transfer/pin/release and stale-owner behavior are deterministic. Lease generation guards PTY resize and engine resize together. Non-owning views clip/scale/letterbox according to their frontend policy; a phone's background connection cannot shrink the working desktop pane.
7. Input authority is separately checked against pane, client/view generation and explicit capability. Read-only clients cannot inject input. Native key events, mouse, focus, bracketed paste and raw-byte operations remain distinguishable. Terminal query replies and clipboard payloads never become user keyboard input.
8. Clipboard authority is per request, originating pane/client, selection, MIME and bounded lifetime, with explicit consent where required. Model synchronous engine callback lifetimes honestly: a pane-local worker may need to pause for a remote response with timeout/cancellation or use an engine-supported continuation. No borrowed callback memory may outlive its call, and no clipboard wait may block unrelated panes or the global runtime. Prove the actual remote consent path before selecting the approach.
9. Local IPC ownership and remote transport authentication are not sufficient by themselves: requests also require scoped read/input/resize/clipboard/control/terminate capabilities. Public metadata, diagnostics and scene exports must not contain owner tokens, broker credentials, forwarded agent secrets or private callback endpoints.
10. Failure semantics are explicit: endpoint disconnect, terminal/process exit, engine fault, storage failure and authorization denial are different events. Neither a stale scene nor successful socket write proves a mutation, harness readiness or processed message acknowledgement.
11. Remote frames and metadata remain untrusted even over authenticated transport. Validate glyph/control-character encoding, coordinate bounds, resource dimensions, decompression limits and links before composition. Scene content cannot emit arbitrary terminal escapes, dispatch local actions or bypass the typed clipboard/query policy; malformed data fails only the affected view.

## 3. Dependency-ordered delivery phases

Each phase closes with recorded evidence and a go/no-go decision. Artifacts belong in the existing shared target tree when implementation begins, not committed throwaway scaffolding. Future work in this worktree uses `CARGO_TARGET_DIR=/home/calops/projects/bkpr/target`; do not create a feature-local target directory.

### Phase 0 — Contract, parity inventory and adoption decisions

Prerequisite: the assessment and this plan are reviewed.

- Inventory current CLI/MCP operations, context/environment fields, recovery schemas, harness launch modes, broker invariants, provider worker ownership, terminal capabilities, UI interaction and packaging paths. Trace exported callers before changing interfaces. Mark each item as preserved behavior, intentional architectural change or explicitly unresolved decision; do not infer support from a package merely building.
- Freeze the module/wire contracts above, including resource limits, snapshot/delta recovery, qualified IDs, ownership, event provenance and grant lifetimes. Write operation-by-operation API migration mappings, including how old tab-oriented commands map to layout/view operations or are intentionally removed in a breaking release.
- Define supported platforms/terminal hosts and a feature acceptance matrix from current behavior, including Kitty graphics and Sixel. Separate client-specific capability degradation from permanent backend data loss.
- Record dependency decisions and the engine spike acceptance criteria. Define test fixture ownership and physical/SSH evidence requirements before coding the spike.
- Update branch contributor/ownership guidance before implementing the new runtime so the current Zellij-specific rules do not become contradictory instructions. Preserve the shipping path's existing safety rules during the transition.

Exit: reviewed contract and traceable acceptance matrix. No engine adoption based only on parsing a shell prompt or drawing a static grid.

### Phase 1 — Physical terminal-engine and PTY feasibility

Depends on Phase 0. Keep this isolated from replacing the shipping launcher.

Use a bounded proof harness for engine rendering, callback scheduling and SSH round trips; the full production frontend and transport arrive later. Keep validated low-level adapters, but remove temporary proof-only launchers and glue after evidence is retained. This phase does not depend on implementing all of Phases 4 and 6 first.

- Build a bounded portable-pty + source-pinned engine adapter experiment: real interactive shell, job control, resize/reflow, scrollback, program exit, alternate screen and wide/combining text; structured snapshot/delta inspection from a separate consumer.
- Prove actual terminal input modes and encoder behavior, cursor shape/visibility, mouse/focus reporting, bracketed paste, keyboard protocols used by supported harnesses, query/reply routing, hyperlinks and terminal-default/palette colors. Unknown or negative capability probes cannot erase an independently confirmed capability.
- Exercise OSC 7501 callback semantics with deterministic emitters and a real reporting harness TUI on a local fake-provider fixture. Semantic status is never inferred from titles, output, progress or MCP activity.
- Exercise Kitty image decode/resource lifetime, placements, clipping, resize, scroll and deletion through the proposed structured scene path and a physical terminal renderer. Include image/text overlap and frontend damage, not just decoded pixel inspection.
- Prove OSC 52 and OSC 5522 text/MIME/image read, write and enhanced paste over a real SSH connection to the client terminal. Exercise allow/deny/delayed consent, malformed/oversized transfers, disconnect and stale client/pane grants. Synchronous C callbacks require measured pane-local scheduling/lifetime behavior, not an impossible blanket promise that callbacks never wait.
- Resolve Sixel explicitly. Current libghostty-vt does not supply application Sixel decoding. Test a maintained, bounded codec/engine integration with real placements, scrolling and resource limits, choose a different engine if justified, or record an explicit product decision about a support change before proceeding. Silent removal or declaring the missing protocol out of scope is not acceptance.
- Prove reproducible native builds and redistribution on the intended platforms, FFI cleanup/callback lifetimes, memory/resource caps and upgrade checks. Bound any proposed upstream contribution by capability and maintenance ownership.

Exit: engine adoption decision with actual physical/SSH evidence and known gaps. Failed adoption reopens this phase; it does not authorize building Bkpr's own terminal engine. This is prototype success only: it does not establish complete frontend, broker or recovery parity.

### Phase 2 — Pane runtime and authoritative registry

Depends on accepted Phase 1 adapter and frozen contracts. Runtime and registry implementation may proceed independently only across those contracts.

- Implement durable pane lifecycle independent of connected views; per-pane process/PTY/engine ownership, bounded event streams, snapshot/delta subscriptions, deterministic teardown and resource accounting. Document daemon-crash behavior rather than promising immortal PTYs.
- Implement layout/session documents, endpoint-qualified references, independent workspace membership and explicit view removal versus termination. Handle the same pane in multiple views and one layout spanning endpoints without proxy ownership cycles.
- Implement size lease selection, pinned dimensions, transfer, expiry/generation checks and per-view chrome exclusion. Add independent input and clipboard capabilities and backpressure/resnapshot handling.
- Persist required metadata/layout intent atomically. Distinguish configured attachment from transient disconnect so explicit removal stays removed after reconnect. Never persist terminal contents, credentials or owner tokens in recovery records without a separately approved change.

Exit: deterministic multi-pane/multi-client lifecycle, concurrency and crash/disconnect fixtures; unrelated panes keep progressing when one client, engine callback or transport is stalled.

### Phase 3 — Typed control, metadata and backend services

Depends on Phase 2 authority; metadata/provider extraction can start earlier against Phase 0 interfaces.

- Move the CLI and MCP dispatcher to exact new identities and capabilities. Preserve explicit terminal input as a separate approval surface from control, no shell interpolation for argv execution, no focus-on-create side effect and no mutation through ambiguous human focus.
- Move CWD/process recognition, Git/watch and forge/provider workers out of plugin/native-session ownership into backend services. Retain canonical Git worktree association, event invalidation, cache restore validation, request batching, user-selected provider account, loading/error distinction and rate-limit/backoff behavior. No provider requests per redraw or search keystroke.
- Publish typed metadata/status events and authoritative live context with provenance. Generic programs remain distinct from recognized harnesses. Frontend annotations are namespaced and cannot fabricate lifecycle or repository facts.
- Preserve stable session-owned repository color assignment across views and metadata refresh; define endpoint-qualified repository identity so unrelated hosts' paths cannot collide. Palette derivation remains per frontend's confirmed capabilities.

Exit: CLI/MCP and metadata fixtures run without a WASI plugin or a Zellij-native owner. Endpoint disconnects and stale generations fail closed rather than falling back to guessed context.

### Phase 4 — Independent projections and full TUI

Depends on Phase 2 frames/views; authoritative UI metadata integrates from Phase 3.

- Implement a real TUI scene renderer and its local chrome: navigation/sidebar, finder, metadata popups, forms, hints, selection/scrollback, links, borders, mouse, keyboard focus and cursor restoration. Reuse widgets for chrome, not a second terminal-state model. Keep overlays client-local; a popup cannot alter another client's focus or pane dimensions.
- Support layouts with splits/weights and client-local selection, resizing and chrome. Initial application-first input behavior must preserve existing shortcut safety without retaining Zellij modes as a hidden dependency.
- Implement a second independently running frontend projection early, preferably a browser consumer of the same structured protocol. Render a live terminal, apply deltas/resnapshot, show different chrome/layout, render a selected single-pane narrow projection, and disconnect/reconnect while the TUI keeps operating. A screenshot viewer, ANSI replay or hard-coded mock is not the proof.
- Demonstrate two views of one pane with different sizes and one mixed-endpoint layout. Only the current eligible size owner resizes the process; read-only and background views do not steal it. Exercise independent input capability grants and a pinned size.
- Prove actual TUI visual/input parity under supported host terminals. Rendering library convenience cannot justify losing graphics, palette behavior, native input or clipboard protocols.

Exit: independent projection proof and TUI acceptance evidence. The browser proof does not yet claim a supported web or mobile product; those require Phase 7's platform work.

### Phase 5 — Harnesses, context, broker and recovery migration

Depends on Phase 3 authority and Phase 2 lifecycle; integrates with Phase 4 UI.

- Bind every shell/direct exec/harness launch before spawn to the new endpoint/session/workspace/pane authority. Preserve immutable launch environment snapshots and live context queries, unmanaged nested-runtime clearing, launch birth checks and stale publisher rejection. Replace native-owner tokens with new private authority; never reinterpret the old token as valid for the new daemon.
- Migrate `bkpr run`, launch-local wrappers and explicitly installed adapters without changing ordinary user profiles, credentials or harness configuration. Keep process creation, recognition, authenticated registration, exact native conversation binding and notification readiness separate. Preserve Codex fresh/resume/fork/picker binding before user submission and its detached app-server rules; no title/latest-thread guessing.
- Preserve harness-native notification paths: active-turn steering where supported and future-turn delivery otherwise. Peer bodies remain untrusted, wakes contain opaque pointers, and no peer message is typed/pasted into a PTY. Test busy, idle, tool failure and not-yet-ready cases with exact native binding.
- Preserve sender-scoped idempotency; durable acceptance, wake acceptance/failure/ambiguity, read and explicit processed ack; one outstanding coalesced wake per launch; stale generation rejection; seven-day post-observed-exit body retention; receipt retention after body removal; and durable-store failure behavior. Do not restart an old broker automatically to hide protocol skew.
- Preserve liveness and cleanup semantics for detached workers, partial/in-flight requests and async wake callbacks. Keep explicit `LOCK_UN` in short-lived RAII advisory leases so fork-inherited file descriptions cannot retain authority. Test process birth/reuse, unload/crash and pending delivery through restart.
- Port stable SSH agent forwarding: latest live managed attachment chooses the upstream agent; unset/no-client fails closed; detaching restores an eligible previous attachment; stable pane socket survives helper restart; generation checks protect in-flight requests. This is separate from size/input/clipboard ownership.
- Migrate recovery ownership, acknowledged snapshots and teardown-removal holdback. Persist layout intent, directories, names and only genuinely captured native harness IDs, not tokens or terminal contents. Refuse competing live owners. Explicit recovery spawns new panes/agents, preserves shell job-control/resume behavior and never adopts an exited launch's inbox.

Exit: end-to-end harness/context/broker/recovery acceptance without real provider calls. A durable send receipt alone, a status badge or successful terminal text is not proof of binding, wake, read or processed ack.

### Phase 6 — Real local and SSH endpoint operation

Depends on Phase 3 control/security and Phase 4 projection; Phase 5 completes agent feature parity.

- Implement the same framed endpoint API locally and over OpenSSH stdio without a transport PTY. Establish endpoint identity and explicit trust/enrollment; preserve SSH host-key checking and user authentication. No public listener or saved-credential change is required for this proof.
- Run panes on genuinely distinct endpoint processes connected through an actual SSH server, including a layout with local and remote panes. Prove process ownership stays remote, layout removal does not kill it, and backend workspace/discovery membership does not follow the viewer.
- Exercise binary data, graphics, input/query round trips, remote clipboard consent, latency/flow control, dropped connections, reconnect/resnapshot, endpoint restart and identity rotation. Offline tiles cannot appear live; reconnect cannot replay ambiguous control/input or consume an expired grant.
- Confirm remote backend Git/provider work uses the selected endpoint's authority and credentials, not the viewing client's repository/path assumptions. SSH agent forwarding remains a separately granted facility.

Exit: retained physical local+SSH interaction evidence and security/authorization fixtures. Loopback transport unit tests are necessary but cannot substitute for this gate.

### Phase 7 — Web/mobile product capability closure

Depends on Phase 4 independent projection and Phase 6 endpoint security. This is explicit frontend work, not functionality supplied by libghostty-vt or a WebSocket library.

This is a separate frontend delivery track. The initial native/TUI cutover needs the independent-projection proof from Phase 4, not a finished web product and native mobile application. Phase 7 remains required before advertising supported web/mobile products; it may ship after the TUI cutover without preserving a permanent Zellij backend.

- Settle the supported frontend/platform matrix using the earlier proof. Implement the web renderer's glyph/font metrics, cursor/styles, selection/scrollback, links, images, clipping, input/IME, pointer/touch, accessibility, reconnect and resource lifetime handling. Choose a maintained renderer only if it can consume the canonical scene without introducing competing terminal authority.
- Implement the mobile projection and required touch navigation, soft-keyboard/IME behavior, single-pane focus, safe resize lease eligibility and background/foreground transitions. Define usable capability degradation explicitly; do not advertise images, enhanced clipboard or input protocols that the platform cannot provide.
- Implement authenticated browser/mobile connection establishment, revocation/expiry and scoped read/input/clipboard/control capabilities. Cover origin checks, browser session/CSRF risks, cross-endpoint delegation, TLS deployment and secret redaction. Default-deny clipboard access and honor platform/user consent.
- Run the declared frontend matrix against the same acceptance scenarios. Frontend-specific chrome is independent; backend lifecycle/status/context and pane authority are identical. No layout projection may masquerade as a backend workspace move.

Exit: declared web/mobile support has real platform evidence and documented limitations. A second-client prototype is not completion of this phase. Any change in intended product support is an explicit decision record, not an unannounced reduction of acceptance scope.

### Phase 8 — Clean cutover and release ownership

Depends on acceptance of Phases 0–6 and Phase 4's independent-projection proof. Phase 7 acceptance is also required for each web/mobile frontend advertised in that release. Do not let a successful demo trigger an early default switch or make an unrequested full mobile application a prerequisite for shipping a complete TUI replacement.

- Complete an operation/feature parity review with every inventory item linked to evidence or an explicitly approved product change. Resolve engine/Sixel and clipboard adoption gates, security review and supported-platform packaging before changing defaults.
- Make the Bkpr-owned runtime the single product path. Remove the development backend selector, obsolete Zellij command adapters, plugin transport, native-owner publication/permissions, runtime-specific assets and superseded tests that the cutover makes dead. Do not preserve a permanent generic all-backends layer or aliases that imply old IDs still authorize new resources.
- Update Cargo workspace/build ownership, `just` recipes, devshell and flake inputs, supported-platform packages, CI/release jobs, cache strategy, installers, bundled control schemas/skills, recovery tooling and developer instructions together. Remove the WASI/native-fork build dependency if no remaining product feature uses it. Review exact licenses, notices, pinned source acquisition and artifact provenance for the new engine/dependencies.
- Update README, AGENTS, changelog, user migration/recovery guidance and architecture documentation. Archive historical native-specific design/evidence as historical where useful and remove obsolete operational instructions/assets. Do not leave two contradictory instructions for launching or owning a pane.
- Give existing Zellij sessions an explicit operational boundary: their running processes remain under their old server until users detach/finish them using the old supported executable. Never hot-replace that server, fabricate new context for it or promise live PTY transfer. New-runtime recovery/import may translate validated declarative layout/directories/names and captured harness resume IDs, but always creates fresh pane/launch identities. Old native IDs, owner tokens and inbox credentials are not migrated as authority.
- Coordinate version/protocol changes and retained broker-store handling with normal release policy. Keep rollback artifacts and declarative export/recovery instructions explicit; rollback does not mean replaying input or reusing invalid authority.

Exit: one maintained runtime path, complete supported-feature evidence, updated packaging/docs and no accidental dependence on the old fork. Shipping only the spike, registry or TUI demo does not satisfy final acceptance.

## 4. Acceptance and verification strategy

All checks below are future implementation work. No build, test, formatter, linter or runtime smoke is exercised by this documentation task. The integration owner runs coordinated checks after slices land; individual workers do not race builds against half-completed shared changes.

| Acceptance area | Required evidence |
| --- | --- |
| Pane lifetime and layout independence | Detach/remove last view, reopen, multi-view/mixed-endpoint layout, explicit terminate, endpoint/process exit and orphan cleanup; durable process identity does not depend on layout slots. |
| Authority and sizing | Deterministic competing focus/visibility/pin/expiry cases, background/read-only clients, narrow mobile foreground/background, separate input and clipboard grants, stale generation/cross-pane rejection. |
| Terminal fidelity | Fixture corpus plus real physical host: alternate screen/reflow/scrollback, Unicode width, cursor, modes, native key/mouse/focus/bracketed paste, queries/replies, hyperlinks, palette/default colors, Kitty image lifetime/placement and Sixel's explicit resolution. No ANSI replay parser in a frontend. |
| Clipboard | Actual SSH text and MIME/image transfer using OSC 52/5522; exact framing/bytes, permission denial/delay, one-use selection/client/pane binding, malformed/oversized/timeout/disconnect cases; no payload reaches keyboard input; unrelated panes continue during consent waits. |
| Status and metadata | OSC 7501 is the only semantic harness-status source; recognition-only/unknown stay unknown. Titles/CWD are independent. Git/provider cache/account/ref invariants and namespaced annotation restrictions have deterministic fixtures. |
| Control and UI | CLI/MCP schema/approval parity, exact-target authorization, argv preservation, no focus-on-create; actual keyboard, physical mouse, focus/cursor, resize, selection, popup and form behavior across independent clients. |
| Agent delivery | Real harness TUI/native adapter with owned fake-provider endpoint, startup/busy/idle/readiness, exact conversation binding, no PTY peer typing, coalescing, idempotency, wake ambiguity, read/processed ack, retention and store failure; inspect authoritative receipts, not terminal success text. |
| Recovery and forwarding | Crash/restart/teardown snapshot races, concurrent recovery refusal, fresh identities, no guessed harness IDs/inbox adoption; stable SSH agent socket, attach/detach/upstream absence and generation races. |
| Remote and frontend support | Actual SSH transport and physical terminal evidence, independent browser/mobile platform runs for declared support, disconnect/reconnect/revocation, authenticated capability boundaries, distinct chrome with shared authoritative state. |
| Resource/security/platform limits | Slow clients, malicious VT/graphics/frame inputs, image/decompression caps, fuzzed framing and callbacks, callback lifetimes, cancellation, storage faults, secret redaction, clean owned-process teardown and supported-platform packaging. |

Use deterministic byte fixtures, captured protocol vectors, property/state-machine tests and fuzz targets for parsing, frame/delta application, lease/grant transitions and recovery. Use fake clocks for retention/expiry and private process/IPC fixtures for birth identity and locks. Keep Linux-specific procfs/pidfd assertions platform-gated rather than weakening production liveness on another platform. Characterize throughput, repaint damage, latency and memory using reproducible workloads and captured timestamps; make no unsupported comparisons or performance promises.

Physical proof uses isolated HOME/XDG, short private socket paths, private local/SSH endpoints and owned processes. Keep raw terminal captures plus frame/event/receipt evidence with versions, dimensions and capability results; redact secrets. Use real maintained harness binaries against deterministic owned fake-provider endpoints with network access restricted to fixtures. Do not call real providers, use the developer's login/configuration, auto-grant harness approvals, touch live sessions or change persistent terminal/harness settings. Existing live-provider smoke scripts are not the default verification strategy for this work. Cleanup must prove owned subprocesses, sockets and temporary credentials are gone while retaining useful evidence.

## 5. Evidence-driven decision records

These decisions are tracked, not questions that block starting the contract/spike work. Resolve them at the named gate and record evidence, chosen option and consequences.

| Decision | Evidence / deadline |
| --- | --- |
| Engine pin, FFI strategy, upgrade owner and supported platforms | Phase 1 callback/render/input/packaging evidence. Reject a large engine fork; evaluate bounded upstream work or an alternative engine. |
| Sixel compatibility path | Phase 1 maintained codec/engine feasibility, physical correctness and resource/security bounds. Any removal requires an explicit product decision before cutover. |
| Synchronous remote clipboard scheduling | Phase 1 engine callback/lifetime API and actual SSH permission behavior. Choose pane-local bounded wait or supported continuation without stalling unrelated panes. |
| Input ownership and collaboration policy | Phase 0 defines safe single-controller default and explicit capabilities; Phase 4 competing-user evidence determines whether any multi-writer mode is justified. Never conflate this with the size lease. |
| Layout persistence and projection-only edits | Phase 0 defines canonical revisioning and backend workspace separation; Phase 4 resolves which local changes are shared versus frontend-private from real multi-client interaction. |
| Orphan pane/session shutdown and resource quotas | Phase 2 lifecycle/resource evidence. Preserve detach survival; make timeout/termination policy explicit instead of treating a missing view as permission to kill. |
| Endpoint enrollment, cross-host workspace/discovery federation and browser authentication | Phase 0 establishes qualified authority; Phases 5–7 prove secure scope/delegation. Viewing a remote pane must never implicitly merge peer-discovery trust domains. |
| Frontend rendering stack and declared support matrix | Phase 4 second-projection and Phase 7 platform evidence; account for IME/accessibility/images/clipboard, not just text drawing. No claim that web/mobile renderers are supplied by the terminal engine. |
| New registry/layout storage and legacy declarative recovery import | Phase 2 transaction needs and Phase 5 recovery evidence. Reuse the broker store; no incidental broker-format migration or old-authority adoption. |
| Exact breaking API/release and old-session transition | Phase 0 operation mapping, finalized in Phase 8 after parity evidence. Clear operational boundary; no live PTY migration or permanent dual backend. |

The implementation owner maintains these decisions and the feature/evidence matrix as work proceeds. Dependency adoption, upstream contributions and product choices remain subordinate to the approved pane-first authority model and the final acceptance gates above.
