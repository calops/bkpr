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
    bash scripts/prepare-ghostty-vt.sh
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
    bash scripts/prepare-ghostty-vt.sh
    export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
    export GHOSTTY_INSTALL_DIR="$BKPR_GHOSTTY_VT_DIR"
    cargo test --locked --workspace
