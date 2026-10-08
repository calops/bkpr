# shephrd

Workspace-first Herdr composition in Zellij. One Rust workspace, two executables: a native launcher/companion and a Rust WASM sidebar.

## Run

From this checkout:

```sh
nix run .
nix run . -- --session project
```

To expose the single command in a Nix profile:

```sh
nix profile install .
shephrd --session project
```

The default package bundles the companion, WASM plugin, Zellij 0.45.1, upstream Herdr 0.9.3, Git and OpenSSH. No Herdr fork is required or modified.

On first launch, click the sidebar and approve its Zellij permission request with `y`. It needs application-state access, application-state changes, command execution and plugin messaging. Authorization is cached by Zellij for the plugin artifact. Select a pane and press Enter to focus its attachment.

The launcher creates a private configuration/layout for this product, not a replacement for your usual Zellij configuration. Application input uses Locked mode; Ctrl+Space enters Zellij control mode and focuses the previous pane. Click the sidebar or use Tab to reach it.

### SSH and saved machines

Install Herdr 0.9.3 on the remote POSIX host first, configure noninteractive OpenSSH authentication, and establish its host key through your normal SSH workflow.

```sh
nix run . -- --remote user@host
nix run . -- --remote user@host --session project
nix run . -- --remote user@host --remote-herdr /absolute/path/to/herdr
nix run . -- --machine saved-label-or-id
```

Remote discovery checks the installed stock bridge capability. Missing selected sessions are started through Herdr's public `remote-client-bridge`; closing that bootstrap connection leaves the detached server alive. There is no remote installation or deployment step. Password prompts and unknown/mismatched host keys fail rather than silently changing SSH policy.

Saved machines use Herdr's existing catalog. A saved profile supplies its session unless `--session` overrides it. To create one with the stock CLI available in the devshell:

```sh
nix develop
herdr machine add user@host --label box --remote-session project
```

Use `--herdr-config /absolute/path/to/config.yaml` for an explicitly selected local Herdr configuration. Enclosing-pane `HERDR_*` variables are not used to select the backend. Executables and sidebar width can be overridden with the flags listed by `shephrd --help`; the bundled wrapper otherwise pins its dependencies.

## Controls

| Context | Key | Action |
| --- | --- | --- |
| Application | Ctrl+Space | Enter control mode; focus previous pane |
| Control mode | Ctrl+Space / Esc | Return to Locked application mode |
| Control mode | Tab / Shift+Tab | Cycle pane focus |
| Control mode | Alt+arrows | Move pane focus |
| Control mode | Ctrl+d | Detach the Zellij client |
| Control mode | Ctrl+q | Quit the Zellij session |
| Sidebar | arrows / j / k | Select row |
| Sidebar | Enter | Focus existing attachment or open one |
| Sidebar | Space | Fold/unfold the selected workspace |
| Sidebar | double click anywhere in a group | Fold/unfold that workspace |
| Sidebar | n | Create workspace |
| Sidebar | N | Create pane in selected workspace |
| Sidebar | r | Rename selected workspace/pane |
| Sidebar | x | Confirm semantic close of selected workspace/pane |
| Sidebar | R | Register/reconnect to the companion |
| Dialog | Tab | Switch workspace-name/directory fields |
| Dialog | Ctrl+u | Clear current field |
| Dialog | arrows / Home / End / Backspace / Delete | Edit Unicode text |
| Dialog | Enter / Esc | Submit / cancel |

Workspace and agent rows have two lines, with blank lines between groups. Workspace details show project/repository names, not branches. Agent harnesses align right; descriptions are muted. Collapsed groups retain their status and name. Selection and hover fill the row width; selecting an agent does not select its workspace. Hover tooltips are terminal-cell floating panes and do not take application input focus.

## Ownership and persistence

Zellij owns the visible composition. Herdr owns logical workspaces, tabs, PTYs, agent detection/status and persistent session state. New logical panes use separate Herdr tabs so the frontend does not reproduce a second tiling owner.

Closing a native Zellij attachment only detaches it. Selecting its sidebar row reattaches to the same Herdr terminal. Explicit sidebar close terminates the selected Herdr pane/workspace after confirmation. Stock direct attachments allow one active client per terminal; this product does not silently take over another client.

Detaching leaves the Zellij server and companion alive. Launching the same target/session reconnects to that UI. Quitting the Zellij session ends its companion, but the headless Herdr server and PTYs remain alive. Stop a persistent backend explicitly with the stock `herdr --session NAME server stop` when it is no longer wanted.

Runtime files live under `$XDG_RUNTIME_DIR/shephrd/<target-session-hash>` or the private state-directory fallback. The local socket/config/logs are current-user-only. The companion starts no system or user service.

## Architecture

```text
Zellij sidebar -> Rust WASM frontend -> native semantic IPC -> stock Herdr API
Zellij terminal -> native shephrd attach -> stock Herdr terminal attachment -> PTY
```

Application bytes never pass through WASM. The frontend receives metadata snapshots and requests semantic actions; it does not replay output snapshots as input or implement another terminal emulator. Local API access uses Herdr's socket; SSH uses its public API bridge. Live events invalidate a cached authoritative snapshot. Project discovery runs asynchronously and uses Git on the appropriate host.

The companion's private IPC is versioned newline-framed JSON with a 4 MiB message limit and peer-UID checks. Snapshot revisions are ordered within a companion instance, not mistaken for Herdr recovery sequence numbers. Zellij state publication streams through CLI-pipe stdin rather than a potentially oversized argv element. The shared contract is in `crates/shephrd/src/lib.rs`.

The WASM plugin keeps its render output separate from stdout-encoded host commands, flushes each frame, memoizes cursor updates and acknowledges CLI input messages. These are required by the pinned Zellij SDK's runtime protocol.

## Develop and build

```sh
nix develop
cargo build --locked -p shephrd
cargo build --locked --release -p shephrd-zellij --target wasm32-wasip1
cargo run --locked -p shephrd -- --session dev
```

The devshell provides the pinned Rust toolchain, WASI target, Rust Analyzer, Clippy, rustfmt, stock Herdr, Zellij, OpenSSH and Git. It selects the local release WASM artifact automatically.

```sh
nix build .#shephrd-companion
nix build .#shephrd-zellij
nix build .#shephrd
nix develop --command cargo test --locked -p shephrd
nix fmt flake.nix
```

Package outputs are exposed for Linux and Darwin on x86_64 and aarch64. Runtime smoke verification was performed on x86_64 Linux with actual Kitty/Xvfb input, Zellij/WASM/native attachments and a strict, isolated loopback SSH server. This is not WAN or Darwin runtime verification.

## Accepted limitations and upstream sequence

This version deliberately uses unmodified Herdr. Its rendered direct-attachment stream does not forward application-origin clipboard sequences, focus-mode notifications or graphics. Zellij/terminal-owned selection copying is a separate path. Enhanced keyboard reporting is also constrained by the stock stack; `Ctrl+b q` remains reserved by Herdr's direct attachment.

The next Herdr work should be general-purpose and upstreamable, in this order:

1. Clipboard: explicit client capabilities and clipboard events forwarded by native attachments, not reconstructed from rendered cells.
2. Focus: client-scoped focus reporting to the selected terminal, respecting application-requested focus mode.
3. Graphics: protocol-aware handling of image state, placement, resizing and reconnect, rather than blind escape-sequence replay.

None of those extensions is implemented here. The existing Herdr fork remains independent.
