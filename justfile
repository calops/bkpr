set positional-arguments

# List available development commands.
default:
    @just --list

# Build the debug launcher and WASI sidebar together.
build:
    cargo build --locked -p bkpr-zellij --target wasm32-wasip1
    cargo build --locked -p bkpr

# Incrementally build the sibling runtime when using the local devshell.
build-native:
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "${BKPR_ZELLIJ_CHECKOUT:-}" ]]; then
        cd "$BKPR_ZELLIJ_CHECKOUT"
        env -u CARGO_TARGET_DIR nix develop --accept-flake-config --command cargo build --locked --offline --no-default-features
    fi

# Build the matching components and launch; CLI overrides remain available.
run *args: build build-native
    #!/usr/bin/env bash
    set -euo pipefail
    if [[ -n "${BKPR_ZELLIJ_CHECKOUT:-}" ]]; then
        export BKPR_ZELLIJ="$(cd "$BKPR_ZELLIJ_CHECKOUT" && pwd -P)/target/debug/zellij"
    fi
    export BKPR_PLUGIN="${CARGO_TARGET_DIR:-target}/wasm32-wasip1/debug/bkpr-zellij.wasm"
    exec cargo run --locked -p bkpr -- "$@"
