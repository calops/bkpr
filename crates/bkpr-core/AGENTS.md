# bkpr-core

## Boundary and entry points

Backend PTYs/processes, panes, authority, registry/layouts, harness/agent/metadata services and durable recovery intent. Depend on protocol and terminal-engine, never client/framework. `Registry::request` and `Pane::request` are trusted in-process APIs; transport supplies IPC authentication.

- `lib.rs`: pane startup, command ordering, published scenes/metadata, native callbacks and owned-child cleanup. `writer.rs` owns ordered bounded PTY writes; `authority.rs` separates view capabilities and desired sizing leases.
- `registry.rs`: hierarchical ownership, atomic creation/close reservations and durable transactions; its services own capture, communication, project/CI workers and recovery.
- `daemon.rs`: one host incarnation/metadata lease and independent session slots. Opening is single-flight by exact logical name; Closing retains the slot through native response/bootstrap drain, socket cleanup and explicit registry lease release. No host-map lock covers startup, storage or teardown.
- `transport.rs`: shared private Unix listener/lease, native framing budgets, startup gate, client lifetimes and decoded-response drain for host and session roles. Only host serving owns signals/readiness. `harness_process` supplies shared off-path observations; Linux `process_tree` retains verified process ownership.
- `notifications.rs` owns bounded host-worker admission, health and resumable waits; `notifications/storage.rs` owns the sole SQLite connection, durable records/policy and volatile episode suppression. Source authentication stays in registry capture. Notification rows never restore live authority or claim provider delivery.
- `diagnostics.rs`: optional body-free observations, never durable acceptance or request outcomes.

## Ownership and publication

- Registry recovery records whitelist intent, never live runtime facts. Lock durable writers transaction → state; release state before filesystem persistence. Publish only after tempfile/fsync/rename and directory fsync. Persistence failure poisons registry health; errors after process side effects do not imply rollback.
- Shutdown drains accepted spawns and preserves recovery intent. Keep explicit lease unlocks: inherited file descriptions can outlive the owner. `RegistryBusy` identifies close reservations, not every spawn conflict.
- Endpoint stop shuts down only registered predecode reader sockets, outside registry/host locks. Reader removal and response-drain admission share one lock; accepted mutation replies must never be canceled as partial readers. Cancel decode-stage waiters and drain canceled readers before releasing a closing session slot.
- Pane command FIFO, view-policy acceptance and clipboard replies have separate synchronization. Native callbacks can block one pane; they must not monopolize policy acceptance or unrelated panes. Clipboard replies bypass command serialization and recheck their exact pending grant.
- Snapshot change tokens are captured before state, so a concurrent update cannot hide behind a newer token. Pane-state revisions, scene revisions, inspection epochs and image generations serve different consumers; do not collapse them. Binary scenes stay outside metadata, with `frame:null` in native metadata. Rejected updates cannot advance consumer baselines.
- Use protocol snapshot/receipt/capability structs, not parallel JSON/DTO definitions. Storage parameters have two concrete consumers: owned client decoding and borrowed/static backend publication. Borrow pane IDs/static receipt labels and use encoding arrays to avoid per-poll allocations. Scene encoding and metadata serialization run outside authority/published-state locks.
- In shared-process tests set `SpawnSpec::isolated_process=false`; dedicated ownership can include adopted children. Preserve immutable process birth and independent root wait/reap ownership even when a cached process sample omits the root.
- Host `daemon_id` is shared; each registry opening gets a fresh `session_instance_id` in addition to its durable `session_id`. Client registration requires all three. Agent scopes, project/CI workers, callbacks and inherited sockets remain session-local; never reap arbitrary host children.
- Routine agent discovery exposes launch/pane identity, explicit discovery labels, semantic state/readiness and matching cached CWD/project-name/branch only. Ownership, status internals and full repository/provider snapshots require explicit detail. Compact projection must not clone rich project snapshots or start Git/provider work.
- Every accepted direct agent message requests a native wake; there is no persisted sender delivery mode. Durable acceptance, current-generation wake claims, native outcomes, read and processed acknowledgement remain distinct. Restart retires old launch inboxes rather than replaying historical quiet or ambiguous wakes; old sender-choice fields disappear on store rewrite.
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

## Verification

Use root Cargo/artifact coordination and host-user execution for permission-sensitive tests. Snapshot tests cover deltas/resource receipts; client `updates::tests` cover missing scenes, revision mismatches and nested JSON rejection. Exercise real PTY input/write receipts and repeated large styled snapshots; allocation evidence is not presentation latency. Run coordinated workspace tests, strict all-target Clippy, formatting and warnings-denied rustdoc.
