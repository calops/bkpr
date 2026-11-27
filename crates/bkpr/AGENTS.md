# bkpr executable

Own user CLI/bootstrap, utility dispatch and native harness adapters; the default terminal application lives in `bkpr-client`. Depend on protocol/client/framework, not backend implementation. `cli.rs` defines public commands; `harness/mod.rs` dispatches native utility arguments before TUI initialization.

The launcher waits for one bounded readiness record on daemon stdout, then checks the private host socket and identity. Closing the readiness pipe cancels the wait, not the daemon. Do not infer readiness from the socket file alone.

`session/administration.rs` owns explicit stop routing independently of the native data-plane family. Inspect and fence the exact host/session incarnation over frozen BKPA v1; never escalate a session stop to a host signal. Pin the mutation connection before observation for known legacy native owners, validate private paths and named manifests, and never replay an uncertain shutdown.

## Harness authority

Managed launches obtain authority through the backend's verified launcher/PTY ancestry checks. Keep capture credentials launch-local; persisted native settings contain code references only. Native bindings, first-prompt facts, notification readiness and OSC semantic status remain distinct. Preserve native interfaces, arguments, consent and unmanaged opt-outs. Codex's private app-server/proxy and OpenCode 2's standalone service are ownership boundaries, not optional installer shortcuts.

`harness/assets.rs` owns private credential files; `integration/shared/native.ts` owns native callback subprocesses. Shared MCP belongs to `harness/agent.rs`; extensions bind native sessions and receivers rather than duplicating tools. Ordinary execution never edits native profiles. Explicit durable operations belong to `harness/install.rs` and its native definitions; preserve unrelated configuration and refuse conflicts. Inspect/dry-run must not create files or invoke onboarding.

`harness/collaboration.rs` owns the exact default native tool names for Claude and Codex. Defaults cannot replace native ask/deny/filter policy or approve control, terminal input or file transfers. Claude defaults stay launch-local alongside the exact verified MCP registration; preserve external MCP sources and never write account-wide name-only grants. Explicit update/uninstall may remove only identifiable legacy owned rules, respecting the schema-1 journal's preserved entries.

Codex thread defaults query the existing private app server through a loopback WebSocket only after admitting an exact fresh user selection. Recheck its connection and sequence after unlocked config I/O. Use Tungstenite's handshake-only build (no TLS); the 0.30 release reuses the workspace's existing `rand` 0.10 and `http` 1 dependencies.

Channel file tools open explicit local paths in the launch-local MCP helper or authenticated Codex relay. Only typed attachment bytes and channel IDs cross the daemon transport; MCP replies carry metadata. Fetch publishes a fresh file without clobbering an existing name.

## Installation boundaries

`install/` describes native files and owned slots; maintained JSONC CST and `toml_edit` preserve unrelated syntax, while Saphyr only reads native Omp YAML policy. Use `url` for native file-URI resolution, not ad hoc escaping. Profile-scoped records bind executable/version and immutable asset hashes. Persistent commands carry `--installation` explicitly because native MCP clients may filter profile/XDG environment variables.

Omp's compiled legacy shim traverses relative imports, not `file:` re-exports; its discovery loader uses `pathdiff` so native package imports are rewritten. Keep the main-context binding guard. Parse TOML documents with `toml::from_str`; `Value::from_str` parses an individual value.

Omp subscriber recovery stays within the selected native binding. Bound automatic retries across short-lived helpers; rearm an exhausted budget only on a new conversation or a root user prompt. Retire queued callbacks and timers on binding changes or shutdown. Reconnection is not permission to reoffer a claimed aside.

Managed execution supplies `BKPR_INTEGRATION_FILE` only after scrub; it names a 0600 grant file in a 0700 launch root, never persistent configuration. Installed commands do not begin capture or discover a daemon. Missing context is inactive; malformed present context fails closed. Keep Codex relay transport and Antigravity native root metadata/context intact.

Installation leases serialize Bkpr writers, not native applications. Sync files and newly created parent directories; compare before writes and conditionally roll back both settings and ownership metadata. Journals contain only declared owned values/hashes, never full user configuration. Keep explicit `LOCK_UN` on lease drop. Bundle cleanup retains referenced/live-leased versions and a bounded previous-version cache; changed or foreign contents are retained with diagnostics.

## Verification

Use the checkout's incremental Cargo target, eight jobs and root instructions. Preserve public executables serving live sessions; freeze matching private debug binaries for native fixtures. Native installation tests use private HOME/XDG, verified raw native executables and fake loopback providers, with no approval seeding or real credentials. Run permission-sensitive tests as the actual host user. A successful configuration write is not proof a native extension loaded or a conversation bound; verify the actual native path and report restart/trust requirements honestly.
