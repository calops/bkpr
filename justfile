set positional-arguments

# List available development commands.
default:
    @just --list

# Prepare the pinned terminal engine and build the native executables (profile: debug or release).
build profile="debug":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
        debug) release=() ;;
        release) release=(--release) ;;
        *) echo "unknown profile: $1 (expected debug or release)" >&2; exit 2 ;;
    esac
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$PWD/target}"
    mkdir -p "$CARGO_TARGET_DIR"
    if ! bash scripts/prepare-ghostty-vt.sh --reuse >/dev/null 2>&1; then
        bash scripts/prepare-ghostty-vt.sh
    fi
    export BKPR_GHOSTTY_VT_DIR="$(bash scripts/prepare-ghostty-vt.sh --print-prefix)"
    export GHOSTTY_INSTALL_DIR="$BKPR_GHOSTTY_VT_DIR"
    cargo build --locked --workspace "${release[@]}"

# Build and run the native daemon, TUI or request client.
run *args: build
    #!/usr/bin/env bash
    set -euo pipefail
    exec "${CARGO_TARGET_DIR:-$PWD/target}/debug/bkpr" "$@"

# Your live sessions are untouched. Rerun with the same name to reattach; a running
# sandbox keeps the binaries it started with until `just sandbox-stop NAME`.
# Extra arguments go to `bkpr tui`, e.g. `just sandbox imgtest release -s work`.
# Open this checkout's build in an isolated bkpr (private daemon, socket and state).
sandbox name="default" profile="debug" *args:
    #!/usr/bin/env bash
    set -euo pipefail
    name=$1 profile=$2
    shift 2
    case "$name" in *[!A-Za-z0-9_-]*|"") echo "sandbox name must be [A-Za-z0-9_-]+" >&2; exit 2 ;; esac
    if (( ${#name} > 16 )); then echo "sandbox name must be at most 16 characters" >&2; exit 2; fi
    # Short, private path: session sockets nest a 64-character hash under it, and Unix
    # socket paths are limited to 107 bytes.
    dir="${XDG_RUNTIME_DIR:-/tmp}/bks-$name"
    if (( ${#dir} + 74 > 107 )); then
        echo "sandbox path $dir is too long for session sockets; set a shorter XDG_RUNTIME_DIR" >&2
        exit 2
    fi
    mkdir -p "$dir"
    chmod 700 "$dir"
    if [[ -x "$dir/bin/bkpr" ]] && "$dir/bin/bkpr" daemon --socket "$dir/c.sock" info >/dev/null 2>&1; then
        echo "Reattaching to running sandbox '$name' (its original binaries; 'just sandbox-stop $name' to rebuild)." >&2
    else
        just build "$profile"
        out="${CARGO_TARGET_DIR:-$PWD/target}/$profile"
        # Copies, so later rebuilds never relink binaries this sandbox is running.
        rm -rf "$dir/bin"
        mkdir -p "$dir/bin"
        cp "$out/bkpr" "$out/bkprd" "$dir/bin/"
    fi
    exec "$dir/bin/bkpr" tui --bkprd "$dir/bin/bkprd" --socket "$dir/c.sock" --state-root "$dir/state" "$@"

# Stop an isolated sandbox's daemon and all its sessions, then delete its directory.
sandbox-stop name="default":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in *[!A-Za-z0-9_-]*|"") echo "sandbox name must be [A-Za-z0-9_-]+" >&2; exit 2 ;; esac
    dir="${XDG_RUNTIME_DIR:-/tmp}/bks-$1"
    if [[ ! -d "$dir" ]]; then echo "no sandbox '$1'" >&2; exit 0; fi
    if [[ -x "$dir/bin/bkpr" ]] && "$dir/bin/bkpr" daemon --socket "$dir/c.sock" info >/dev/null 2>&1; then
        "$dir/bin/bkpr" daemon --socket "$dir/c.sock" stop
    fi
    rm -rf "$dir"

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
