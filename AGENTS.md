# Maintenance notes

## Replacement branch

This branch replaces the previous Bkpr implementation. It is not a compatibility track. Zellij, WASI, legacy launchers and their regression obligations may be removed; do not maintain two mux authorities or preserve obsolete interfaces merely to keep old tests passing.

`main` and the retained `calops/bkpr-zellij` fork are starting material, not specifications. Inspect their current relevant implementations when a subsystem comes into scope. Reuse useful code, refactor weak boundaries and replace designs where better ones fit the agreed product. Keeping coherent useful modules immediately is allowed; copying dormant legacy trees merely for preservation is not useful. Git history preserves source material.

Record source revision/path and licensing when copying substantial code. Preserve applicable copyright/license notices, including Zellij's MIT notice. Reusing rendering, terminal capability, input or clipboard work does not require retaining Zellij's runtime architecture.

The agreed direction is a native pane-first backend with independent clients. Panes own processes/PTYs and canonical terminal state. Layouts reference panes rather than owning their lifetime. Clients may project layouts differently, including a single-pane mobile view and cross-endpoint composition. Input, sizing and clipboard permissions remain separate. Do not claim unimplemented product capabilities or treat an experiment as production acceptance.

## Product and authority

- Prefer maintained dependencies for terminal parsing, PTYs, codecs, networking, storage and UI infrastructure. Own Bkpr's agent/harness behavior, lifecycle, metadata and authority policies rather than a parallel generic SDK. Keep foreign-source changes small and contribute reusable gaps upstream where practical.
- Recognized harness status comes from OSC 7501, not titles, output, progress or process presence. Process/CWD recognition and titles are separate metadata.
- Do not change user profiles, credentials, persistent third-party settings or GitHub accounts during normal execution. Explicit installation must preserve unrelated settings and refuse conflicts.
- Clipboard transport belongs to the native backend/client boundary. Preserve exact MIME bytes and framing; replies must never become keyboard input. Grants bind to their original pane, client, selection and offer. Timeouts or malformed transfers must not fabricate contents.
- Agent identities must be authenticated and launch-scoped. Discovery, registration, native binding, actual subscriber readiness, durable acceptance, wake, read and processed acknowledgement are distinct. Never use PTY typing as peer delivery or blindly replay ambiguous input/wakes.
- Unknown ownership or context fails closed. Persist no tokens or terminal contents in recovery records. Durable storage errors must not be hidden by successful-looking acknowledgements.
- Associate repositories with foreground CWD and canonical common Git identity; worktrees have independent HEADs. Never fetch merely for UI. Preserve authoritative remote/PR commit identity separately from local HEAD, and respect provider spacing/backoff and the user's active account.
- Use terminal-default colors and confirmed per-client capabilities. Avoid fixed themes and semantic claims inferred from presentation. Frontends must not overwrite backend-owned lifecycle, status or repository facts with annotations.

These are product constraints, not a mandate to copy old APIs, storage formats, schedulers or implementation tests. Design concrete contracts as their subsystem is implemented.

## Development and verification

Use `nix develop` and the native build/run instructions in README. No Zellij or WASI build is required on this branch. Run coordinated native checks after edits: workspace tests, workspace/all-target strict Clippy, formatting, and an actual smoke of the changed path. Tests of deleted implementations are not acceptance criteria.

Reuse the existing worktree and canonical shared Cargo target. Do not create per-feature Cargo caches or discard incremental artifacts. Source changes here must not mutate `main`, sibling checkouts, installed programs or running user sessions. Protect live canonical executable artifacts when sharing output paths; use a private output view with shared caches or preserve a known-good immutable executable outside build output.

Use isolated HOME/XDG, private sockets, explicit owned process identities and fake local providers for fixtures. Never use real model/provider services or mutate live sessions for smoke tests. Test terminal input, pointer, resize, graphics, focus and clipboard behavior on the actual terminal surface where affected. Clean up owned processes and temporary files; retain useful evidence under `target/smoke/`.

Keep procfs, verified Linux process-birth and subreaper helpers/tests Linux-only. Do not weaken ownership checks to make portable tests pass. Native fixtures must not use the developer's HOME or process-global environment mutation; use private per-fixture roots and nonblocking PTY reads.

## Commits and releases

Use Conventional Commits. Mark actual breaking changes with `!` and a `BREAKING CHANGE:` footer; do not label maintenance as a feature merely to force a release. Automatic releases remain in `0.x`; changing to `1.0` requires deliberate user approval. Do not publish experimental builds as production-ready or retain obsolete release plumbing as a successful no-op.
