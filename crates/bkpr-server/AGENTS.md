# Daemon bootstrap boundary

This package builds `bkprd`; it has no public Rust library API. `src/main.rs` parses CLI configuration, binds one core host daemon, installs optional host diagnostics and serves it. Keep pane/process authority, session supervision, persistence, restoration and request dispatch in `bkpr-core`; shared request/configuration types belong to `bkpr-protocol`. Do not introduce a second backend implementation here.

## Startup and shutdown contracts

- `HostDaemon::bind` acquires one host metadata/socket lease; named registries retain separate durable leases and private sockets. Live listeners and metadata owners must not be displaced or adopted.
- `--socket`/`--state-root` select a private host namespace. Optional `--session` opens a headless initial session; explicit argv without a name selects the default session and authorizes bootstrap/restoration. No name and no argv starts an empty host. Missing/invalid defaults must not make headless control unavailable or replay historical programs.
- `--home` selects core's scrubbed fixture environment, not an OS sandbox. Use a separate empty fixture HOME; GUI tools can populate their own HOME before the daemon initializes its isolation marker.
- Session-local harness/forwarding services are configured before session bootstrap. `--bkpr` supplies the companion utility; otherwise it is resolved adjacent to the daemon executable. Never use an incompatible or live debug helper in a fixture.
- Final workspace closure and session `Shutdown` retire only that session; zero-session hosts remain idle. Explicit expected-ID `DaemonShutdown`, SIGINT and SIGTERM drain all sessions through normal owned cleanup. Core installs process signals only on host control.
- The core host owns one notification worker and drains it after sessions. Corrupt/unavailable notification storage exposes explicit health without preventing session execution; unsafe ownership or a competing writer fails closed. Bounded drain failure never claims durable success. Historical records/cursors survive session closure and never restore live harness authority.
- Diagnostics are host-local and best effort, tagged by host incarnation. Failure to install diagnostics does not authorize successful control/persistence or skipped cleanup.

## Dependencies and verification

Keep the executable thin: Anyhow supplies error context and Clap derives CLI/help. `serde_json` and Tempfile are test-only direct dependencies. Core still uses JSON internally; moving the server declaration does not remove JSON from its transitive closure or establish a build-time reduction. Retain useful Clap help/error presentation rather than removing features by count alone.

`tests/startup.rs` runs real private hosts covering multi-session identity/isolation, explicit/headless startup, restoration, last-close transitions, host shutdown and failed-start cleanup. `BKPR_TEST_SERVER` selects the frozen matching daemon for host-user runs. The ignored `owned_native_fixture` is entered only by its owned subprocess test, with a private HOME marker; never run it directly or against a live daemon. Its copied native shell blocks on a retained stdin pipe, without fork/exec polling that can destabilize foreground-process recognition.

Follow root instructions for protected product builds, host-user tests and coordinated workspace checks. Exercise actual `bkprd --help` for CLI documentation and private startup/control/shutdown for lifecycle work. Do not build against public executable paths used by live sessions. No predecessor Zellij daemon needs to be preserved: this package is the native service entrypoint, not a compatibility backend.
