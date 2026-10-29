# Remote workspaces implementation plan

Status: superseded architecture alternative, 2026-10-29. Do not execute this Zellij-specific plan for the pane-first backend. Use the [current implementation plan](pane-first-mux/implementation-plan.md); this document is retained for the earlier proposal's research and compatibility findings.

Specification: [remote-workspaces-design.md](remote-workspaces-design.md).

## Delivery boundary

Deliver native session federation with one backend-owned tab per displayed tab, one frontend sidebar, real native interaction and safe disconnect/reconnect. Include multiple local sessions, SSH-hosted sessions and explicit remote-session creation with normal permission grants. Do not ship an SSH-in-a-pane substitute, text-only output viewer, auto-grant path or stubbed control layer as completion.

The expensive unknown is native scene export/import, not the host-profile UI. Prove that first. If a proof requires a broad upstream rewrite, amend the spec and discuss the fork cost before continuing.

## Repository and branch discipline

- Bkpr: use the existing checkout and incremental `target/`; do not replace a binary/plugin being used by live helpers. Preserve an external known-good runtime for comparison where needed.
- Native fork: use a retained `feature/remote-workspace-presentation` branch when the current work is safely committed/available. Do not switch or stash away another task's uncommitted edits. If genuinely concurrent work needs a separate checkout, use `~/.local/share/bkpr/worktrees/bkpr-zellij/` with the canonical native `target/`, not a sibling feature worktree or fresh Cargo cache.
- The current fork checkout has unrelated uncommitted resize/output/client-scoped-surface changes. Record their merge state and coordinate with their owner before integrating. They are not a landed dependency and must not be bundled into the remote feature.
- Native features go through retained PR branches into fork `main`, human validation and explicit merge commits. No auto-merge, squash/rebase merge or release-update CI for an ordinary feature PR.
- After the required fork PRs land, update Bkpr using `nix flake update bkpr-zellij`, retaining the fork's dependency pins. Ordinary legacy sessions remain attachable.
- New module paths below are proposed placements, not claims of existing APIs. Before changing exported Rust symbols, use language-server references and migrate all callers. Prefer small additions to current contracts over parallel dispatch conventions.

## Dependency order and parallel work

| Task | Depends on | Integration output |
| --- | --- | --- |
| 0. Native feasibility spike | None | Source-backed hook inventory, physical proof and agreed presentation boundary. |
| 1. Freeze contracts and identities | 0 | Versioned wire schema, target model, limits, action ownership and compatibility rules. |
| 2. Backend scene export and view leases | 1 | Native source attachment with declared chrome exclusion and safe lifecycle. |
| 3. Frontend native tab surface and routing | 1, then 2 for integration | Native scene import, input routing, geometry, cursor and local overlays. |
| 4. Host supervisor and helper transport | 1, then 2/3 for integration | Local IPC and SSH transport, validated backend access, bounded flow control and reconnect. |
| 5. Metadata/control federation | 1, then 4 for integration | Qualified snapshots, endpoint-aware control and backend-local authority. |
| 6. Terminal resources and device interactions | 2, 3, 4 | End-to-end graphics, clipboard, palette/capabilities and correlated terminal queries. |
| 7. Frontend-driven backend startup permissions | 2, 3, 4 | Real remote-session creation without bypassing plugin permission grants. |
| 8. Sidebar, finder and public commands | 3, 4, 5, 7 | Complete enrollment/create/attach/navigation/operation/disconnect UI and CLI. |
| 9. View persistence and recovery | 4, 5, 8 | Backend-reference persistence without remote layout/process restoration. |
| 10. Integrated verification and release cutover | 2 through 9 | Physical/SSH evidence, coordinated checks, docs/changelog and pinned native feature. |

After Task 1, source export, frontend surface, transport and metadata/control can be independent work slices with fixed contracts. Task 6's resource contract is designed in Task 1, not bolted on after a text demo. One integration owner owns wire changes, integration branches and acceptance. Agents must skip build/lint/test/format commands mid-flight; run coordinated checks after integration, not concurrently in shared Cargo targets. Targeted physical/protocol proof runs occur at the spike and integrated feature boundaries.

## Task 0: Prove native composition with minimal upstream hooks

**Purpose:** turn the proposed boundary into evidence before building profiles, UI or a broad remote-control layer.

**Read first:**

- Fork `zellij-server/src/output/mod.rs`: `Output`, `CharacterChunk`, Kitty/Sixel chunks and final serialization; cursor and pre/post VTE need explicit treatment.
- Fork `zellij-server/src/tab/mod.rs`, `screen.rs`, `tab/mouse_handler.rs`, `route.rs`: render selection, resize participation, native actions and client cleanup.
- Fork `zellij-client/src/input_handler.rs`, `stdin_handler.rs`, `lib.rs`: key/reply parsing, mode state and the sole terminal writer.
- Fork `zellij-utils/src/ipc.rs` and `client_server_contract/`: attachment negotiation and protobuf message framing.
- Bkpr `crates/bkpr/src/lib.rs`, `crates/bkpr-zellij/src/native.rs`, `focused.rs`, `overlay.rs`: declared sidebar roles, backend owner, current local overlays and focus.

**Work:**

- [ ] Identify the precise source scene-export point and frontend scene-import point before ANSI serialization. Enumerate every ordinary pre/post terminal instruction; classify it as scene state, typed terminal request or unsupported capability, never arbitrary wire ANSI.
- [ ] Establish a per-client declared chrome exclusion using native Bkpr role metadata. Keep another ordinary source client attached and prove its sidebar, panes and layout remain intact, apart from documented shared resizing.
- [ ] Bind one exact backend tab to a frontend content rectangle beside the actual sidebar. Exercise a shell, an editor, local finder/creation overlays, native cursor, physical pointer and a raster image without a second VT grid.
- [ ] Measure source/effective/destination geometry with two differently sized clients, changing sidebar width and another viewer's presence. Prove which clients cap which tabs and how suspension releases only that view's size participation.
- [ ] Trace key dispatch on both servers. Choose exact interception points for local chrome actions versus backend content actions; prove one keymap application, not two.
- [ ] Drop/recreate the presentation connection while recording backend shell PID/birth. Prove full-frame reactivation, stale-epoch rejection and view-lease replacement without detaching another viewer.
- [ ] Produce a table of upstream files changed, hooks added, why each hook cannot live entirely in a new module, and interactions with existing fork features. Record physical evidence under `target/smoke/remote/`.

**Gate:** no feature implementation until all five blocking proofs in the spec have evidence and the hook inventory is accepted. A throwaway spike is not shipped production infrastructure. Remove its scaffolding after preserving useful evidence.

## Task 1: Freeze qualified targets, presentation schema and ownership

**Proposed new Bkpr modules:** `crates/bkpr/src/remote_contract.rs` for host/WASI-safe identities and metadata; `remote/` for native-only runtime code. Keep the existing single-backend `Snapshot` valid rather than stuffing host prefixes into canonical pane IDs.

**Proposed native additions:** `zellij-utils/src/presentation.rs` and dedicated presentation protobuf schema/messages. Update only needed module exports and IPC conversions. Do not derive the public wire by serializing internal `Output` objects.

- [ ] Specify `EndpointId`, session incarnation, workspace/tab/pane/agent references, frontend view handle, view ID, connection/geometry epochs and owner-local snapshot revision.
- [ ] Specify full and delta frames, base-sequence acknowledgement/resync, cursor, resolved colors/links, resource chunks/releases, input/actions, results, host-query replies and lifecycle messages.
- [ ] Set byte limits, resource budgets, queue windows and scheduling priorities from spike observations, including image payloads. Ensure decoder validation precedes allocation.
- [ ] Set compatibility rules for helper and native schemas, required capabilities and unsupported old servers. Preserve ordinary legacy native attachment and current private owner-token boundaries.
- [ ] Specify native action ownership and endpoint restrictions for operators versus authenticated agents. A CLI `--host` flag cannot promote an MCP caller to operator.
- [ ] Specify tab/view versus backend workspace grouping. Several backend tabs can share a workspace/session; a presentation grouping grants no cross-host peer membership.

**Behavioral regressions worth keeping:** colliding IDs across hosts, owner/connection/geometry epoch changes, out-of-order deltas, oversized resources, unknown required capabilities and authorization-changing selector combinations. Do not add permanent tests for schema field copies or source text.

**Exit:** contracts suffice for the independent slices below without choosing terminal behavior independently.

## Task 2: Backend presentation export, declared chrome and view leases

**Proposed native modules:** `zellij-server/src/presentation/{mod,source,scene,geometry,lease}.rs`. Select the final module split after Task 0; avoid an empty abstraction per file.

**Expected upstream hooks:** client attach/capability dispatch in `route.rs`; selected-tab render and client lifecycle in `screen.rs`; scene contribution in `tab/mod.rs` and `output/mod.rs`; native IPC schema/conversions. No `RemotePane` variant spread through all pane implementations.

- [ ] Attach a presentation client to the exact session/tab and native source geometry. Bind its lifecycle to the source server incarnation and private view lease.
- [ ] Export a self-contained, resolved scene excluding only declared frontend chrome for that client. Continue ordinary source rendering untouched.
- [ ] Exclude presentation-only tabs from bindable backend topology and reject direct `AttachView` attempts to export them. Mutual session attachments cannot create proxy chains or recursively expanding sidebar entries.
- [ ] Keep PTY parsing/layout/scrollback native-owned. Avoid output queues coupled to program write rate; the export queue represents latest view state.
- [ ] Implement full/delta scene state with correct damage, clears, cursor, link and image-resource lifecycle. Backend-owner metadata remains a separate channel.
- [ ] Replace/suspend/detach only the specified view generation. Removing a presentation client never closes source panes. A same-name new session cannot inherit the old view.
- [ ] Implement explicit effective-size negotiation using current shared native layout semantics. No independent per-client layout engine.

**Exit:** source export is sufficient for R2-R7 and preserves an ordinary source client under R4/R10. Native protocol fixtures must not use the developer's HOME or global environment mutation.

## Task 3: Frontend native tab surface and interaction routing

**Proposed native modules:** `zellij-server/src/presentation/{local_tab,compositor,input}.rs`, with narrow client-side query/reply integration where required.

**Expected upstream hooks:** eligible tab rendering when no local selectable terminal exists, content bounds, action routing after local chrome priority, cursor composition, geometry change and client cleanup. Use one common dispatch boundary rather than tests in every native action implementation.

- [ ] Create a backend-tab binding with local chrome and one native external-session surface. Do not create local PTYs or mirrored terminal-pane identities for backend panes.
- [ ] Import resolved scene data into existing native `Output`, below local overlays. Scope damage/clears to content bounds and preserve local sidebar/gutter rendering.
- [ ] Implement an explicit input owner state: local focused overlay; sidebar control; remote content ready; remote content unavailable. Physical terminal replies follow correlation, not keyboard focus.
- [ ] Route native content actions and application input to the bound backend exactly once. New tabs/splits/run operations cannot fall through to local process creation. Unsupported actions fail before changing anything.
- [ ] Translate mouse/selection/hyperlink coordinates using the current geometry epoch. Backend pane separators and floating content keep normal behavior; local overlay hit-testing wins.
- [ ] Switch sources atomically: retire old cursor/resources, clear exposed content, request a full new scene and enable content input only after ready. No stale frame can appear after switching back to local content.
- [ ] Reuse this path for another local session through private IPC; the frontend's own local tabs keep their normal render/action path.

**Exit:** R1-R4 work with local IPC before SSH is blamed for presentation bugs. This is an integration checkpoint, not feature completion without graphics/clipboard and remote coverage.

## Task 4: Native host supervisor and SSH/local helper transport

**Proposed Bkpr modules:** `crates/bkpr/src/remote/{profiles,supervisor,transport,helper}.rs`; main hidden stdio-helper dispatch and explicit public entry points. Reuse current runtime-root, atomic-write, owned-socket and process-birth/lease validation patterns.

- [ ] Add explicit Bkpr-owned host profiles with stable IDs. Keep labels/SSH aliases separate from endpoint authority; no automatic alias deduplication.
- [ ] Support direct local IPC for another local session and system SSH `-T` to the fixed remote helper for a remote endpoint. Quote the fixed executable/helper command for the remote POSIX shell; put all dynamic targets in framed messages.
- [ ] Resolve only managed same-user native sessions remotely. Attest live owner/birth locally on that host; never send private owner tokens or relay credentials to the frontend.
- [ ] Keep transport SSH noninteractive with strict host-key checking and no controlling terminal. Missing trust/authentication must become an actionable UI state, never a password/host-key prompt behind native rendering. No password/askpass broker or automatic host-key enrollment is part of this feature.
- [ ] Negotiate helper/native protocol and required presentation support before view creation or mutation. Handle missing binaries, noisy stdout and incompatible versions without installing or restarting anything.
- [ ] Bind each physical frontend client's active view to its own source presentation client. A native supervisor may share metadata/control transport, but cannot share interactive focus or clipboard state between clients.
- [ ] Implement byte-bounded transport and diagnostics, bounded frame/resource queues, ordered input, query priority and cancellation. Never let a stalled viewer block the remote server's PTY reader or unrelated clients.
- [ ] Reconnect an established exact backend with a fresh epoch. Retire only the old lease; re-sync full display state. Never replay buffered input or user mutations. Surface unknown outcomes distinctly from known rejection.
- [ ] Respect normal SSH host-key/authentication policy. No implicit `-A`, persistent SSH changes or credential copying. Preserve the existing latest-live-managed-attachment SSH-agent policy; metadata-only subscribers must not claim that attachment role. Decide the exact supervisor/agent integration during this task and prove it with private socket fixtures.
- [ ] Own child cleanup and socket unlink by exact instance. Graceful frontend detach ends viewers/helpers, not remote native servers/programs. Hung SSH must not stall local work or leave adopted cleanup survivors.

**Exit:** R6/R7 hold under actual SSH, not only an in-memory tunnel. SSH mutation/input acknowledgements refer to backend acceptance, not success writing to a local pipe.

## Task 5: Credential-free metadata and endpoint-aware control

**Affected existing modules:** Bkpr `lib.rs`, `context_contract.rs` only where needed, `control/{arguments,operations,native}.rs`, `crates/bkpr-zellij/src/native.rs`; native helper runtime gets a new federation coordinator rather than embedding SSH in WASI.

- [ ] Publish backend public topology/project/status snapshots with owner-local epochs/revisions and explicit connection freshness. Keep native ownership/context validation on the owning host.
- [ ] Federate qualified records without overwriting current single-session topology caches or accepting cached metadata as fresh native manifests.
- [ ] Route operator-selected endpoint operations through the same typed dispatcher on that host. Do not implement a second copy of workspace/pane business rules in the frontend.
- [ ] Preserve exact backend client selection for focus/mode/detach and shared floating/fullscreen restrictions. A frontend native client ID is not a backend client ID.
- [ ] Treat remote paths/Git/CI as remote. Run provider queries on the owning host with current spacing/backoff/authentication rules. Qualify repository IDs before frontend color assignment.
- [ ] Keep agent launch/context/discovery/message behavior backend-local. Remote UI agent IDs are endpoint-qualified display/control references, not relayed bearer tokens or evidence of cross-host messaging support.
- [ ] Maintain caller authority and unknown-outcome receipts. Refuse foreign endpoint targets from an authenticated agent unless a separately specified authorization supports them; never silently reroute to a local target.

**Exit:** R1/R8 plus input/control parts of R2/R6/R7; same names, paths and IDs on two hosts cannot select the wrong program or provider identity.

## Task 6: Complete resources and physical-terminal semantics

**Modules:** native presentation resource/composition modules and existing client terminal query/reply integration. Extend proven clipboard/grant policy; do not move it into Bkpr WASI.

- [ ] Namespace image content/placement lifecycle by source/view generation, allocate physical IDs locally and compose Kitty/Sixel under native clipping and overlay occlusion.
- [ ] Preserve cursor shape/visibility, Unicode/wide/combining characters, selection highlights and resolved hyperlinks across partial updates and source switches.
- [ ] Propagate per-physical-client confirmed capabilities, palette/default colors and pixel dimensions. Preserve terminal-native fallback and avoid conflicting host background/palette writers.
- [ ] Correlate remote host queries/replies and clipboard transfers to exact source pane, client/view, epoch, deadline and selection. Preserve MIME bytes/framing, permissions, cancellation and timeout behavior. Clipboard replies are never keyboard input.
- [ ] Invalidate resources and grants on geometry/view replacement, source shutdown and reconnect without deleting another source's state. Keep retained-state memory within negotiated budgets.
- [ ] Validate remote scenes before composition: coordinate bounds, safe glyph/style/link encoding, resource dimensions and decoded-byte budgets. Crafted cells cannot smuggle terminal escapes or dispatch local actions; malformed data fails only the affected view.

**Exit:** R3-R5/R6 under actual terminal capabilities. Text-only screenshots or mocked clipboard echoes are not proof.

## Task 7: Explicit remote startup and plugin permissions

**Prerequisite source:** the background-first permission issue already recorded in `TODO.md`; native plugin lifecycle and permission handling must be traced before changing it.

- [ ] Fix pending lifecycle replay to real plugin instances, visible pending permission prompts on later attachment and temporary prompt sizing/restoration. Keep normal cached grants and explicit approval/denial.
- [ ] Add a launcher-owned provisional bootstrap capability for the exact initiating remote-create operation. It permits attaching to startup/permission presentation, not publishing authoritative workspace metadata or spawning a harness before owner readiness.
- [ ] Start a remote managed session only on explicit create, serialized against conflicting create/resume/attach records. A failed handshake/discovery or reconnect must never create one.
- [ ] Present the real remote native startup and permission UI through the native surface. On grant transition to the live native-owner capability; denial/cancellation remain visible and cannot be treated as ready.
- [ ] Define cancellation outcomes explicitly: detach the provisional view, report whether the session was created, and offer an explicit stop if needed. Never blindly kill a possibly established backend or retry an ambiguous create.

**Exit:** R11 plus R7/R10. Backend startup is not delivered while the current permission blocker remains or requires manual cache edits/auto-grants.

## Task 8: Complete sidebar/finder and CLI workflows

**Affected modules:** current sidebar `main.rs`, `native.rs`, `ui.rs`, `pane_search.rs`, focused-overlay content; Bkpr CLI/control schemas and new remote module. Reuse focused transport/controller and existing typed dispatch, not a second modal framework.

- [ ] Add the documented `remote add/list/remove/create/attach/disconnect` and endpoint-aware session/control commands. Generate exact help/schema/completion coverage from normal CLI definitions.
- [ ] Support the reserved `local` endpoint over private IPC for public session list/attach/disconnect. Reject enrollment/removal of that endpoint, reuse the frontend's own direct binding and reject remote-disconnect of its owning native session.
- [ ] Add host/session selection and startup/permission/error states to focused frontend forms. Keep the common sidebar responsive during remote latency/authentication failure.
- [ ] Render local and remote workspace records together, preserving workspace-first grouping, stable endpoint-qualified selection and distinct connection/harness states. Do not flatten remote panes into fake local native terminals.
- [ ] Support live backend tab creation/removal/reorder and selection of the exact pane/tab. Keep multiple tabs per backend workspace/session.
- [ ] Extend finder metadata with `host:` while preserving existing filters, links, project colors and client-scoped navigation. Remote file/path actions cannot open an unrelated local path.
- [ ] Give destructive backend close/stop and harmless frontend disconnect/removal different commands and confirmation text. Native close still terminates actual backend programs; view removal never masquerades as application close.
- [ ] Keep ordinary local shortcuts, plugin permissions and Locked attachment unchanged. Foreign/stale view frames cannot control another attached client's sidebar or overlays.

**Exit:** the complete daily workflow is usable without knowledge of hidden helpers or a command-palette feature.

## Task 9: Persist endpoint profiles and view references

**Affected modules:** new remote profile/view store, existing recovery contracts/writer/restore path only where integration requires them.

- [ ] Persist private host profiles, endpoint/session/tab references and desired attachment state atomically in XDG config/state. Keep credentials, private native authority and terminal output out of the store.
- [ ] Choose a separate owner-guarded view-reference record or a versioned recovery-record variant. Preserve existing local recovery records and monotonic authority; a proxy surface is never restored as a local shell/harness.
- [ ] On frontend restart/recovery validate exact backend incarnation and capabilities before reconnecting only desired-connected references. Preserve explicit disconnect intent across restart for both local IPC and SSH; stale callbacks cannot re-enable it. Offline and ended targets remain actionable records, not automatic backend creation/resume.
- [ ] Keep remote backend recovery independent. Normal frontend quit/recovery/removal cannot replay remote pane trees or harness resume commands.
- [ ] Remove disconnected references explicitly without destructive backend operations. Crash/teardown must not erase previously acknowledged view references accidentally.
- [ ] Acknowledge explicit disconnect only after durable desired-state update; distinguish it from inactive-view suspension and transient transport failure. Cover disconnect, crash/restart and a delayed reconnect callback as one lifecycle regression.

**Exit:** R9 with live, offline and same-name-replaced backend sessions, while existing local recovery remains correct under R10.

## Task 10: Integrated evidence, checks, documentation and cutover

Build through the project devshells and incremental Cargo artifacts. Use Bkpr `just build` / `just run --zellij ../bkpr-zellij/target/debug/zellij` and the fork's devshell/offline debug build. Do not use Nix packaging builds for iteration. Never replace artifacts serving a live-session fixture; select explicit binaries/plugins and retain a known-good runtime outside build output.

**Physical/SSH acceptance matrix:**

| Scenario | Observe | Spec acceptance |
| --- | --- | --- |
| Two local sessions plus two remote sessions with colliding labels, pane IDs and paths | Exact selected backend, shell PID, native tab and Git/provider source; create/split/close and native shortcut targets. | R1, R2, R8 |
| Shell, full-screen editor and fake recognized harness emitting deterministic OSC 7501 | Physical input, Kitty keyboard, bracketed paste, cursor, selection, mouse and local overlay focus; no real providers. | R3, R10 |
| Two physical clients at different cell/pixel sizes plus an ordinary backend client | Independent tab selection/modes, shared effective layout, inactive-view sizing release, no disappearing backend sidebar. | R4 |
| Terminal-default/ANSI/truecolor and Kitty/Sixel capable clients | Palette inheritance, images, overlays, source-switch clipping/resource cleanup and hyperlink coordinates. | R5 |
| Distinct clipboard MIME data/selections with delayed, malformed and stale replies | Correct original pane/client/generation; no fabricated content or typed reply bytes. | R5, R7 |
| Network loss, stalled frame consumer, fragmented/oversized frames and reconnect | Backend PID survival, bounded memory, responsive local work, lease replacement, full resync, no repeated input/mutations. | R6 |
| Missing/incompatible helper, rejected/changed host key, owner-lease loss and same-name replacement | Visible failures before unsafe operations, no installs/restarts/config/credential mutations. | R7 |
| Explicit startup with uncached permission, denial and cancellation | Real visible prompts, no ready state or harness launch before grant, no blind create retry. | R11 |
| Frontend quit/recovery with live, offline and ended backends | Validated view references only, no remote relaunch/termination; explicit remote close still terminates. | R9 |
| Known-good older session and current local-only workflow | Ordinary legacy attach, native splits/tabs, clipboard, per-client Locked startup and recovery remain usable. | R10 |
| Public local-IPC attachment and mutual A-to-B/B-to-A attachments | Reserved-local CLI behavior, only backend-owned application tabs exported, no proxy recursion or misrouted operations. | R1, R7 |
| Explicit disconnect followed by frontend recovery, over local IPC and SSH | Reference retained but automatic attachment disabled; delayed callbacks cannot reconnect it. | R9 |

Use `target/smoke/remote/` for throwaway controllers and retained sanitized evidence. Use short private runtime roots, isolated HOME/XDG and multiplexer/credential environments, thread-local native fixtures and owned process cleanup. Devshell Python/pyte controllers use `sys.executable`. Interact with actual isolated terminal surfaces for cursor, focus, physical pointer, resize and graphics; a pyte shadow screen alone cannot validate them. Use an owned SSH fixture/VM, never live sessions or real providers. Do not change global network/SSH policy to simulate faults.

After integration, run coordinated checks once:

```sh
# Bkpr devshell
cargo test --locked --workspace
cargo build --locked -p bkpr-zellij --target wasm32-wasip1
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo clippy --locked -p bkpr-zellij --target wasm32-wasip1 -- -D warnings

# Fork devshell, canonical incremental target
cargo build --locked --offline --no-default-features
```

Run the fork's focused existing tests for changed attachment/output/input/lifecycle paths and its supported Clippy invocation after the native changes are integrated; record the exact commands/results rather than assuming a workspace-wide command is appropriate. Permanent new regressions cover consumer-visible transitions, bounds and authority errors, not source text, copied wire fields or mock forwarding echoes.

- [ ] Complete every spec acceptance row with observed evidence and record any actual platform/terminal limit. An unexercised remote host is not production SSH verification.
- [ ] Inspect and reduce the native upstream-file diff against the accepted hook inventory. Remove spike scaffolding, dead paths and any unneeded broad refactors.
- [ ] Update README user workflows, endpoint/permission/close semantics, supported-version/platform limits and troubleshooting. Add a changelog entry after behavior is proved. Cross-host agent messaging remains explicitly unsupported.
- [ ] Submit/validate the retained native PR(s), merge manually, then update the Bkpr pin without dependency overrides. Commit Bkpr as a user-visible feature only when end-to-end behavior is present.

## Main risks to resolve early

1. **Structured export may be larger than it looks.** `Output` owns native stores and contains raw pre/post terminal operations. An extra serialization method alone does not produce a safe remote scene.
2. **Tiled chrome exclusion is not current floating-layer scoping.** Prove it beside another backend client, including geometry changes and permission overlays.
3. **Native mode/action ownership crosses client and server code.** A shortcut handled twice or on a local fallback can create/close the wrong program.
4. **Size participation is shared backend state.** Do not introduce independent reflow accidentally or let hidden viewers pin a remote tab small.
5. **View leases and device grants have asynchronous teardown races.** Epoch checks must cover both frames and callbacks, not only metadata.
6. **Fresh backend startup already has a permission-lifecycle prerequisite.** It must be fixed visibly, not avoided with warmed caches.
7. **Upstream merge cost can exceed code size.** Check hook locality and interactions with retained fork features, not only added/deleted line counts.

These are implementation gates, not reasons to substitute a smaller feature under the same name.
