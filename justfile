set positional-arguments

# List available development commands.
default:
    @just --list

# Prepare the pinned terminal engine and build the native bkpr executable.
build:
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
    mkdir -p "$CARGO_TARGET_DIR"
    if ! bash scripts/prepare-ghostty-vt.sh --reuse >/dev/null 2>&1; then
        bash scripts/prepare-ghostty-vt.sh
    fi
    export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
    export GHOSTTY_INSTALL_DIR="$BKPR_GHOSTTY_VT_DIR"
    cargo build --locked --workspace

# Build and run the native daemon, TUI or request client.
run *args: build
    #!/usr/bin/env bash
    set -euo pipefail
    exec "${CARGO_TARGET_DIR:-$PWD/target}/debug/bkpr" "$@"

# Prepare the same pinned engine and run the native workspace tests.
test:
    #!/usr/bin/env bash
    set -euo pipefail
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
    mkdir -p "$CARGO_TARGET_DIR"
    if ! bash scripts/prepare-ghostty-vt.sh --reuse >/dev/null 2>&1; then
        bash scripts/prepare-ghostty-vt.sh
    fi
    export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
    export GHOSTTY_INSTALL_DIR="$BKPR_GHOSTTY_VT_DIR"
    cargo test --locked --workspace

# Capture the isolated README demo using an existing matching native pair.
screenshot bkpr_bin="target/debug/bkpr" bkprd_bin="target/debug/bkprd" output="target/showcase/site/showcase.png" frontend_bin="":
    #!/usr/bin/env bash
    set -euo pipefail
    args=(--bkpr "$1" --bkprd "$2" --output "$3"
          --dummy-key-consent allow --security-notice-consent allow
          --fixture-project-trust allow --fixture-broker-tools allow)
    if [[ -n "$4" ]]; then args+=(--frontend "$4"); fi
    exec nix develop .#showcase --command python3 scripts/showcase/capture.py "${args[@]}"
