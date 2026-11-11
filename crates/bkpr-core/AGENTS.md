# bkpr-core

## Boundary and entry points

Backend PTYs/processes, panes, authority, registry/layouts, harness/agent/metadata services and durable recovery intent. Depend on protocol and terminal-engine, never client/framework. `Registry::request` and `Pane::request` are trusted in-process APIs; transport supplies IPC authentication.

- `lib.rs`: pane startup, command ordering, published scenes/metadata, native callbacks and owned-child cleanup. `writer.rs` owns ordered bounded PTY writes; `authority.rs` separates view capabilities and desired sizing leases.
- `registry.rs`: hierarchical ownership, atomic creation/close reservations and durable transactions; its services own capture, communication, project/CI workers and recovery.
- `transport.rs`: private Unix listener/lease, native framing budgets, startup gate, client lifetimes and decoded-response drain. `harness_process` supplies shared off-path observations; Linux `process_tree` retains verified process ownership.
- `diagnostics.rs`: optional body-free observations, never durable acceptance or request outcomes.

## Ownership and publication

- Durable records whitelist intent, never live runtime facts. Lock durable writers transaction → state; release state before filesystem persistence. Publish only after tempfile/fsync/rename and directory fsync. Persistence failure poisons registry health; errors after process side effects do not imply rollback.
- Shutdown drains accepted spawns and preserves recovery intent. Keep explicit lease unlocks: inherited file descriptions can outlive the owner. `RegistryBusy` identifies close reservations, not every spawn conflict.
- Pane command FIFO, view-policy acceptance and clipboard replies have separate synchronization. Native callbacks can block one pane; they must not monopolize policy acceptance or unrelated panes. Clipboard replies bypass command serialization and recheck their exact pending grant.
- Snapshot change tokens are captured before state, so a concurrent update cannot hide behind a newer token. Pane-state revisions, scene revisions, inspection epochs and image generations serve different consumers; do not collapse them. Binary scenes stay outside metadata, with `frame:null` in native metadata. Rejected updates cannot advance consumer baselines.
- Use protocol snapshot/receipt/capability structs, not parallel JSON/DTO definitions. Storage parameters have two concrete consumers: owned client decoding and borrowed/static backend publication. Borrow pane IDs/static receipt labels and use encoding arrays to avoid per-poll allocations. Scene encoding and metadata serialization run outside authority/published-state locks.
- In shared-process tests set `SpawnSpec::isolated_process=false`; dedicated ownership can include adopted children. Preserve immutable process birth and independent root wait/reap ownership even when a cached process sample omits the root.

## Dependencies and diagnostics

Retain [portable-pty](https://crates.io/api/v1/crates/portable-pty/0.9.0), [crossbeam-channel](https://crates.io/api/v1/crates/crossbeam-channel/0.5.17), [Mio](https://crates.io/api/v1/crates/mio/1.2.4) (`net`, `os-poll`), notify/ignore and scraper (defaults disabled) for their existing infrastructure roles. No new dependency is needed for typed publication. Cargo-tree duplicates stem from engine SHA build dependencies and upstream PNG/compression/proc-macro generations; no consolidation was justified here. [fs2 0.4.3](https://crates.io/api/v1/crates/fs2/0.4.3) was released in 2018; modernization needs explicit-unlock, contention and fork/descriptor-lifetime verification.

Diagnostics reuse bounded crossbeam admission and an idle receiver. Preserve descriptor-relative/no-follow opens, private ownership/modes, hard-link rejection and retained byte/count bounds. Install after state-root ownership, before panes. Shutdown must not join a potentially blocked filesystem worker. [tracing-appender](https://docs.rs/tracing-appender/0.2.5/src/tracing_appender/worker.rs.html) lacks retained-byte/private-open policy; [file-rotate](https://docs.rs/file-rotate/0.8.0/file_rotate/) owns path opening and documents discarded filesystem errors. This justifies small policy glue, not a generic logger.

## Verification

Use root Cargo/artifact coordination and host-user execution for permission-sensitive tests. Snapshot tests cover deltas/resource receipts; client `updates::tests` cover missing scenes, revision mismatches and nested JSON rejection. Exercise real PTY input/write receipts and repeated large styled snapshots; allocation evidence is not presentation latency. Run coordinated workspace tests, strict all-target Clippy, formatting and warnings-denied rustdoc.
