# bkpr-core

## Boundary and entry points

Backend PTYs/processes, panes, authority, registry/layouts, harness/agent/metadata services and durable recovery intent. Depend on protocol and terminal-engine, never client/framework. `Registry::request` and `Pane::request` are trusted in-process APIs; transport supplies IPC authentication.

- `lib.rs`: pane startup, command ordering, published scenes/metadata, native callbacks and owned-child cleanup. `writer.rs` owns ordered bounded PTY writes; `authority.rs` separates view capabilities and desired sizing leases.
- `registry.rs`: hierarchical ownership, atomic creation/close reservations and durable transactions; its services own capture, communication, project/CI workers and recovery.
- `daemon.rs`: one host incarnation/metadata lease and independent session slots. Opening is single-flight by exact logical name; Closing retains the slot through native response/bootstrap drain, socket cleanup and explicit registry lease release. No host-map lock covers startup, storage or teardown.
- `transport.rs`: shared private Unix listener/lease, native and frozen administration framing budgets, startup gate, client lifetimes and decoded-response drain for host and session roles. `transport/administration.rs` compares exact immutable identity and endpoint role before stop admission, reusing normal owned cleanup even with poisoned persistence. `capacity::read_incoming` recognizes `BKPA` before strict native-family admission; both share byte/decode/response gates and cancellation, never a second listener or generic JSON fallback. Only host serving owns signals/readiness. `harness_process` supplies shared off-path observations; Linux `process_tree` retains verified process ownership.
- `notifications.rs` owns bounded host-worker admission, health and resumable waits; `notifications/storage.rs` owns the sole SQLite connection, durable records/policy and volatile episode suppression. Source authentication stays in registry capture. Notification rows never restore live authority or claim provider delivery.
- `channels.rs` owns per-session SQLite storage and bounded subscriptions. Presets use ordinary channel rows; create/open attempts run outside registry locks and discovery repairs missing rows without changing existing rows. Workspace retirement does not take the channel writer on the close path.
- Channel user authority checks the authenticated owner's lifetime at admission and excludes daemon descendants even through mixed-UID SSH ancestry. Cross-UID topology is not harness process authority. Archive scans bound candidates separately from returned rows; continue on `next_after` even for empty pages. Only verified access loss or lifecycle closure ends a subscription successfully; storage and registry failures propagate.
- Unposted PNG uploads expire after 24 hours and share a 64 MiB per-session budget. An indexed staging ledger and transactional byte counter bound admission; expiry cleanup runs on upload, never on a workspace close. Posting promotes references in the same transaction as history and releases their staging quota.
- `diagnostics.rs`: optional body-free observations, never durable acceptance or request outcomes.

## Ownership and publication

- Registry recovery records whitelist intent, never live runtime facts. Lock durable writers transaction → state; release state before filesystem persistence. Publish only after tempfile/fsync/rename and directory fsync. Persistence failure poisons registry health; errors after process side effects do not imply rollback.
- A saved channel participant is workspace-scoped intent paired with a native recovery descriptor. Only a matching reserved resume from the verified pane-root launcher transfers it. Live admission polls the retained kernel exit witness. Retirement signals the channel writer to reconcile only pending work; it never transfers membership rows, inboxes or wakes to a new launch.
- `registry/agents/channel_wakes.rs` claims committed work through the existing native receiver. Channel batches share its generation order and slot arbitration with direct messages, but keep their receipts in SQLite. Reserve the slot under the agent writer and release runtime locks before SQLite writes. Only committed targets update per-launch subscription hints; neither idle checks nor terminal/input work query channel storage.
- A recovery title is a bounded display label, never source authority. Capture only against the bound foreground process, exact conversation and queued-read generation. Keep the last admitted title across shell return, coalesce spinner-only changes, and save through the existing observer/completion writer. Explicit pane labels win.
- Shutdown drains accepted spawns and preserves recovery intent. Keep explicit lease unlocks: inherited file descriptions can outlive the owner. `RegistryBusy` identifies close reservations, not every spawn conflict.
- Endpoint stop shuts down only registered predecode reader sockets, outside registry/host locks. Reader removal and response-drain admission share one lock; accepted mutation replies must never be canceled as partial readers. Cancel decode-stage waiters and drain canceled readers before releasing a closing session slot.
- Pane command FIFO, view-policy acceptance and clipboard replies have separate synchronization. Native callbacks can block one pane; they must not monopolize policy acceptance or unrelated panes. Clipboard replies bypass command serialization and recheck their exact pending grant.
- Pane color publication requires the exact live size owner at admission and at native application. Reserve capacity for any scheme report before native mutation, then commit its FIFO packet after releasing view authority. Rejection leaves the scheme and defaults untouched. Keep observed full and ANSI16 bases only for the last publishing owner, separate from the preset; a fresh full palette subsumes earlier ANSI16 evidence. An unchanged target updates owner/evidence without calling setters or extracting a frame.
- Native application OSC overrides survive committed default changes. Snapshot override metadata enables inferred previews to preserve detectable overrides; the SDK cannot distinguish an override equal to its default RGB. Read the metadata with the scene, including synchronized output holds.
- Snapshot change tokens are captured before state, so a concurrent update cannot hide behind a newer token. Pane-state revisions, scene revisions, inspection epochs and image generations serve different consumers; do not collapse them. Binary scenes stay outside metadata, with `frame:null` in native metadata. Rejected updates cannot advance consumer baselines.
- Use protocol snapshot/receipt/capability structs, not parallel JSON/DTO definitions. Storage parameters have two concrete consumers: owned client decoding and borrowed/static backend publication. Borrow pane IDs/static receipt labels and use encoding arrays to avoid per-poll allocations. Scene encoding and metadata serialization run outside authority/published-state locks.
- Polls accept `planus_v1` and `planus_v1_sources`. The latter sends an image's retained original PNG/zlib payload (see the engine's retained sources) instead of pixels; both omit generations the poll reports as held. Image resource requests are retired.
- In shared-process tests set `SpawnSpec::isolated_process=false`; dedicated ownership can include adopted children. Preserve immutable process birth and independent root wait/reap ownership even when a cached process sample omits the root.
- Linux periodic discovery walks pane descendants; the ownership sampler walks daemon descendants, including adopted children, plus exact retained members. Child edges are checked against the parent's birth before admission. Unsupported task `children` lists, opaque processes and traversal errors fall back to a full scan. Omissions alone cannot prove exit or withdraw a published harness: foreground changes from a scoped sample are confirmed on a fresh full table. Shutdown checkpoints and root reap also use full snapshots; launch retirement independently verifies birth and ancestry. The 100 ms ownership and 200 ms discovery intervals are unchanged.
- Host `daemon_id` is shared; each registry opening gets a fresh `session_instance_id` in addition to its durable `session_id`. Client registration requires all three. Agent scopes, project/CI workers, callbacks and inherited sockets remain session-local; never reap arbitrary host children.
- Routine agent discovery exposes launch/pane identity, explicit discovery labels, semantic state/readiness and matching cached CWD/project-name/branch only. Ownership, status internals and full repository/provider snapshots require explicit detail. Compact projection must not clone rich project snapshots or start Git/provider work.
- Every accepted direct agent message requests a native wake; there is no persisted sender delivery mode. Durable acceptance, current-generation wake claims, native outcomes, read and processed acknowledgement remain distinct. Restart retires old launch inboxes rather than replaying historical quiet or ambiguous wakes; old sender-choice fields disappear on store rewrite.
- A claimed direct wake batch accepts new messages for at most 30 seconds from claim, regardless of native outcome; reading, acknowledgement and authority changes may release it sooner. Channel batches have fixed membership and a 32-post cap. Expiry never makes claimed work eligible for another pointer. Exact retries don't touch either batch.
- Direct-message selectors may be exact launch IDs or uniquely eligible live root-conversation IDs. Fingerprint the caller's original selector/text/scope, but persist the resolved exact launch in `Message.to`; retained digests keep retries stable after rebinding, ambiguity, retirement and body expiry without a store migration. Both retry checks precede lookup. At admission, hold registry transaction → agent writer and fence the entire sampled candidate set, binding authority/generations and current scoped ownership; process/foreground sampling stays outside those locks. Retired exact IDs never fall through aliases, and ambiguity exposes only bounded in-scope launch/pane pairs.
- Message ownership linearizes at validated insertion. Drop the registry transaction before JSON/fsync; retain the agent writer as the binding fence, with no later recipient re-resolution.
- `registry/notification_capture.rs` authenticates every canonical engine status replacement before frame/status coalescing. Per-pane capture gates serialize native/OSC observations and bounded nonblocking host-service admission, never durable writers. Foreground process birth, exact live managed generation and native enrollment precedence remain required; clear, EOF and replacement withdraw only. Feed epochs prevent mid-chunk authority replacement from reattributing old records. Engine callbacks release their `RefCell` borrow before entering the registry; this path uses cached facts, never process scans.

## Dependencies and diagnostics

Retain [portable-pty](https://crates.io/api/v1/crates/portable-pty/0.9.0), [crossbeam-channel](https://crates.io/api/v1/crates/crossbeam-channel/0.5.17), [Mio](https://crates.io/api/v1/crates/mio/1.2.4) (`net`, `os-poll`), notify/ignore and scraper (defaults disabled) for their existing infrastructure roles. No new dependency is needed for typed publication. Cargo-tree duplicates stem from engine SHA build dependencies and upstream PNG/compression/proc-macro generations; no consolidation was justified here. [fs2 0.4.3](https://crates.io/api/v1/crates/fs2/0.4.3) was released in 2018; modernization needs explicit-unlock, contention and fork/descriptor-lifetime verification.

Diagnostics reuse bounded crossbeam admission and an idle receiver. Install once under the host root with the host incarnation, never under whichever session opened first. Preserve descriptor-relative/no-follow opens, private ownership/modes, hard-link rejection and retained byte/count bounds. Shutdown must not join a potentially blocked filesystem worker. [tracing-appender](https://docs.rs/tracing-appender/0.2.5/src/tracing_appender/worker.rs.html) lacks retained-byte/private-open policy; [file-rotate](https://docs.rs/file-rotate/0.8.0/file_rotate/) owns path opening and documents discarded filesystem errors. This justifies small policy glue, not a generic logger.

Notifications use maintained MIT `rusqlite` 0.42.0 with defaults disabled,
`bundled` SQLite and `fallible_uint` checked integer conversions. This adds a C
build and six locked packages, not an async runtime/system SQLite requirement.
Only actual notification/policy/loss/pruning changes write storage; working,
duplicate and disabled reports stay disk-free. A concrete next-expiry timer
enforces retention while otherwise idle. Corrupt storage is explicitly unavailable
without poisoning registry execution; unsafe ownership or another writer remains
a startup error. Admission is not durable acceptance. Cursor loss generations
acknowledge gaps without hiding historical loss or causing permanent busy waits.
Bounded shutdown drains accepted work or reports failure while a stalled worker
retains its store lease. Do not reuse agent inbox notifications or acknowledgements.

Push enrollment and its outbox share that same connection and notification-acceptance
transaction (`notifications/push_store.rs`). No history cursor feeds a second pipeline.
Only explicit authenticated host control manages destinations; pending encrypted nonce
challenges expire after ten minutes. Confirmation stores a SHA-256 proof, while the
private queued challenge payload is cleared when its attempt becomes terminal.
Rotation preserves the logical destination ID, keeps the old generation active until
proof, then atomically retires its queued jobs. Filter updates retain the proven
subscription while advancing its generation. Enrollment, update and test request IDs
deduplicate exact request fingerprints for seven days, with the horizon in each
receipt. The 4096-entry/4-MiB ledger rejects admission until old receipts expire rather
than prematurely forgetting a promised key. After that horizon, inspect durable
destination/job state before retrying an uncertain mutation. Retired generations
compact to 128; a separate secret-free highwater preserves monotonic generations for
every retained logical destination. Subscription descriptors separately have a
4-MiB bound; at most 32 logical destinations and 32 pending generations are live.
Jobs retain their own compact payload independently of history pruning: 512 pending
host-wide, 64 per destination, four independent network workers and 128 recent terminal
diagnostics. Overflow commits explicit per-destination loss with the source record.
Five-minute event/test TTLs, at most six attempts and provider minimum retry deadlines
bound retries. Unknown, local rejection and provider acceptance never replay blindly.
Retired generations retain public labels/filter settings while removing subscription
capability URLs, recipient/auth keys and proof hashes. Renewal of an expired current
generation uses the same logical ID and a new encrypted challenge; explicit revocation
cannot be renewed. Empty destination kinds mute automatic events without removing the
proven route; explicit tests remain available. Idempotency stores request fingerprints,
not original secret-bearing enrollment bodies. Private SQLite retention does not imply
secure erasure of old pages or journals. The immutable committed master PEM is cached
as an `Arc<str>`; no job rereads or copies it after the first load.
The scheduler is lazy and deadline-driven; full workers do not arm due-now queue
timers. Shutdown cancels network futures and retains dispatching work as unknown;
restart never treats an interrupted dispatch as safe to resend. No source authority,
phone delivery or display is inferred from durable history or HTTP acceptance.

## Verification

Use root Cargo/artifact coordination and host-user execution for permission-sensitive tests. Snapshot tests cover deltas/resource receipts; client `updates::tests` cover missing scenes, revision mismatches and nested JSON rejection. Exercise real PTY input/write receipts and repeated large styled snapshots; allocation evidence is not presentation latency. Run coordinated workspace tests, strict all-target Clippy, formatting and warnings-denied rustdoc.
