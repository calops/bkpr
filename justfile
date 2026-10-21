set positional-arguments

# List available development commands.
default:
    @just --list

# Build the debug launcher and WASI sidebar together.
build:
    cargo build --locked -p bkpr-zellij --target wasm32-wasip1
    cargo build --locked -p bkpr

# Build both components, then run with the matching plugin and Bkpr arguments.
run *args: build
    BKPR_PLUGIN="${CARGO_TARGET_DIR:-target}/wasm32-wasip1/debug/bkpr-zellij.wasm" cargo run --locked -p bkpr -- "$@"
