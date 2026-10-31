# TODO

The native replacement branch is governed by the [implementation plan](docs/pane-first-mux/implementation-plan.md). `main` and the Zellij fork are starting code, not immutable designs or regression suites. Reuse, refactor or rewrite relevant pieces when their domain comes into scope.

Current priority is TUI feature parity with `main`, starting with harness capture. Browser/web, GUI and mobile frontends are deferred; an independent frontend proof must not block restoring the TUI's product functionality.

- [ ] Complete the pane-first backend and independent clients: durable pane identities/lifecycle, revisioned layout documents, backend workspace membership, view/size/input/clipboard authority, typed control, rich metadata and agent/harness services. Use maintained infrastructure libraries and small source ports rather than importing a whole mux. The initial [engine experiment](docs/pane-first-mux/engine-proof.md) records measured behavior and unresolved rendering/input observations, not full product acceptance.

- [ ] Restore automatic harness capture and TUI identity/status end to end: foreground harness/CWD recognition in ordinary panes, managed launch-scoped identity, exact native conversation binding, and independent OSC 7501 semantic status. Reuse or redesign main's supported launchers and installed adapters without requiring different everyday commands or rewriting user settings. Preserve explicit launch, resume/fork/noninteractive behavior and recognition-only launches; clear live capture on process replacement/exit without losing recovery metadata. Publish typed backend facts and verify genuine harness TUIs with isolated local fixtures. This is the next implementation slice, not functionality already supplied by the terminal status parser.

- [x] Replace Bkpr's generic Ghostty FFI ownership with pinned upstream `libghostty-rs` safe/sys crates and a reviewed compatible native ABI. Keep Bkpr's scenes, bounds, caches and authority policy; record the reproducible local dependency patch and exact native provenance.

- [ ] Submit the narrow native paste-offer provenance contribution and reusable SDK fixes upstream where practical, then remove local patches as matching upstream versions land. Track parser-boundary ABI updates, exact prebuilt linking, optional image data and pre-allocation PNG budgets explicitly.

- [ ] Reduce sustained-animation CPU cost after the bindings/architecture cutover. Matched 240×79 `cmatrix` panes used about 1.47 old Bkpr / 1.52 new Bkpr CPU cores versus 0.17 for Zellij. The new real-sidebar window is larger; observed software-framebuffer cadence is not identical. One-span damage now avoids repainting unchanged row prefixes/suffixes, but the efficiency gap remains. Profile native extraction, compact encoding, composition and drawing separately; investigate finer disconnected-span damage and redundant work without lowering fidelity or silently throttling updates. Retain `target/smoke/workspace-cutover/performance/` and the earlier `target/smoke/release-performance/` evidence; rerun matched release workloads and explicit geometry controls.

- [ ] Evaluate useful rendering/device work from the retained Zellij fork as the corresponding new-client feature is implemented: per-client capability/palette state, damage and wide-glyph handling, layer/cursor/pointer visibility, image-resource lifecycle and origin-bound clipboard replies. Retain applicable source notices; choose better designs where appropriate rather than preserving fork plumbing.

- [ ] Add a first-class worktree workflow: discover/open linked worktrees, create from existing/new branches, and switch recent worktrees. Connect selection to workspace and tool/harness launches. Setup/teardown commands must be explicit and reviewable, never automatically run repository-provided commands. Confirm destructive removal and distinguish canonical repository identity from worktree branch/CWD. Reference: <https://muxy.app/docs/2.x/features/projects#worktrees>.

- [ ] Add a command palette for searchable actions, workspace/pane navigation, shortcuts and saved commands. Share typed action dispatch with CLI/MCP and UI controls. Keep action search distinct from metadata search; scope focus per client and confirm destructive operations. The old overlay implementation may supply useful code but isn't required architecture.

- [ ] Implement remote panes/workspaces with actual local and SSH endpoints in one layout. Execution, terminal state and authoritative metadata stay on the owning endpoint; layout/view state does not confer workspace or agent authority. Design enrollment, authentication, framing, cancellation, reconnect/resnapshot and stale-grant handling explicitly. No session-per-tab restriction or SSH-in-a-pane substitute. Reference: <https://muxy.app/docs/2.x/features/remote-servers>.

- [ ] Explore opt-in persistent terminal history: readable/searchable screens and scrollback after pane exit or backend restart. Define storage budgets, retention, deletion, encryption/privacy requirements and a clear distinction from recovery metadata; output may contain secrets. Reference: <https://muxy.app/docs/2.x/features/server#history>.

- [ ] Design recovery selection and harness resume behavior against the new registry. A picker should distinguish live panes from recoverable executions. Investigate whether harnesses already restore model/permission/launch flags before persisting them. Reuse useful main recovery/adapter code, but do not inherit its session IDs, KDL schema or native-owner model as authority.

- [ ] Implement harness launch/context and peer communication against the new backend. Inspect main's real Claude/Codex/Omp/Antigravity integrations before choosing reuse versus rewrite. Bind genuine conversations, preserve truthful readiness and durable delivery/processed acknowledgement, and never type notifications into PTYs. Test startup, active/idle turns, crashes, store failures and installation conflicts with isolated fake local providers.

- [ ] Design SSH agent forwarding separately from input, size and clipboard ownership. Reuse useful lease/generation and raw Unix-path work from main; make selected-client/upstream policy explicit. Do not mutate user SSH/Git configuration to hide routing or authorization failures.

- [ ] Explore a bounded cross-instance cache for expensive decorative raster assets if measurements justify it. Reuse suitable strip/corner/color-resolution ideas from the fork, but keep native image IDs, placements and live upload state client-local. Use versioned keys, atomic writes, resource limits and XDG cache storage.

- [ ] Explore passive agent lifecycle animation after static chrome and resource lifetimes are stable. Main/fork bee/hive assets and prior evidence may be useful; artwork, trajectories, scheduling and fallback can be redesigned. Animation must not delay process termination, infer harness status or steal cursor/focus/pointer ownership. Measure actual terminal support and performance before choosing native animation versus frame updates.

- [ ] Profile new-client popup presentation before optimizing it. Measure damage, placement, visibility and readiness across keyboard/pointer handoffs; don't carry old plugin/native round trips into the new design unnecessarily.

- [ ] Revisit integration across direnv/environment transitions when harness launch is implemented. Main's wrapper-free installer and explicit launch paths are useful starting points; avoid rewriting shell behavior or direnv's saved environment as a workaround.

- [ ] Restore release/package publication only after reproducible offline engine/toolchain acquisition, dependency/license review and supported-platform native verification. No legacy runtime artifacts or no-op success workflows.
