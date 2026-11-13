# Daemon bootstrap boundary

This package builds `bkprd`; it has no public Rust library API. `src/main.rs` parses CLI configuration, opens the registry, installs optional diagnostics, configures services and runs transport bootstrap. Keep pane/process authority, persistence, restoration and request dispatch in `bkpr-core`; shared request/configuration types belong to `bkpr-protocol`. Do not introduce a second backend implementation here.

## Startup and shutdown contracts

- `Registry::open_named` acquires metadata ownership before binding control transport. A live listener or metadata lease must not be displaced. Socket paths must be absolute; core validates private directories and ownership.
- Explicit argv after `--` authorizes bootstrap/restoration using the current command, HOME and CWD. Without argv, startup executes nothing. Daemon `SHELL` can configure defaults for future panes, but missing/invalid defaults must not make headless control unavailable or replay historical programs.
- `--home` selects core's scrubbed fixture environment, not an OS sandbox. Use a separate empty fixture HOME; GUI tools can populate their own HOME before the daemon initializes its isolation marker.
- Harness and agent-forwarding services are configured before `serve_bootstrap`. `--bkpr` supplies the companion utility; otherwise it is resolved adjacent to the daemon executable. This is not permission to use an incompatible or live debug helper in a fixture.
- Shutdown runs after bootstrap, bind or service failures as well as normal serving. Core owns child teardown and last-pane/workspace/session transitions. An initially empty headless daemon is not a last-workspace-close transition.
- Diagnostics are daemon-local and best effort. Failure to install diagnostics does not authorize reporting successful control/persistence or skipping owned cleanup.

## Dependencies and verification

Keep the executable thin: Anyhow supplies error context and Clap derives CLI/help. `serde_json` and Tempfile are test-only direct dependencies. Core still uses JSON internally; moving the server declaration does not remove JSON from its transitive closure or establish a build-time reduction. Retain useful Clap help/error presentation rather than removing features by count alone.

`tests/startup.rs` runs real private daemons covering explicit/headless startup, restoration, last-close transitions and failed-start cleanup. `BKPR_TEST_SERVER` selects the frozen matching daemon for host-user runs. The ignored `owned_native_fixture` is entered only by its owned subprocess test, with a private HOME marker; never run it directly or against a live daemon. Its copied native shell blocks on a retained stdin pipe, without fork/exec polling that can destabilize foreground-process recognition.

Follow root instructions for protected product builds, host-user tests and coordinated workspace checks. Exercise actual `bkprd --help` for CLI documentation and private startup/control/shutdown for lifecycle work. Do not build against public executable paths used by live sessions. No predecessor Zellij daemon needs to be preserved: this package is the native service entrypoint, not a compatibility backend.
