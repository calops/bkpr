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

# Open an isolated development instance: just sandbox NAME -- [BKPR ARGS...].
# Set BKPR_SANDBOX_PROFILE=release to use a release build.
sandbox name="default" *args:
    #!/usr/bin/env bash
    set -euo pipefail
    name=$1
    shift
    if [[ ${1:-} == -- ]]; then shift; fi
    profile=${BKPR_SANDBOX_PROFILE:-debug}
    case "$profile" in debug|release) ;; *) echo "expected debug or release profile" >&2; exit 2 ;; esac
    dir=$(just sandbox-dir "$name")
    if [[ -e "${XDG_RUNTIME_DIR:-/tmp}/bks-$name" ]]; then
        echo "Sandbox '$name' uses the previous layout. Use another name to keep its data, or 'just sandbox-stop $name' to remove it." >&2
        exit 2
    fi
    if [[ ! -e "$dir" && ! -L "$dir" ]]; then mkdir -m 700 "$dir"; fi
    if [[ ! -d "$dir" || -L "$dir" || ! -O "$dir" ]]; then
        echo "Sandbox directory must be owned by you and not a symlink: $dir" >&2
        exit 2
    fi
    export XDG_RUNTIME_DIR="$dir" XDG_STATE_HOME="$dir/state-home"
    export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-8}"
    unset BKPR_SOCKET
    bin="$dir/bin/bkpr"
    socket="$dir/bkpr/d"
    if [[ -x "$bin" ]] && "$bin" daemon --socket "$socket" info >/dev/null 2>&1; then
        echo "Using running sandbox '$name' and its original binaries." >&2
    else
        just build "$profile"
        out="${CARGO_TARGET_DIR:-$PWD/target}/$profile"
        # Copies, so later rebuilds never relink binaries this sandbox is running.
        rm -rf "$dir/bin"
        mkdir -p "$dir/bin"
        cp "$out/bkpr" "$out/bkprd" "$dir/bin/"
    fi
    if [[ ${1:-} == run && ${2:-} != --help && ${2:-} != -h && $# -ge 2 ]]; then
        if [[ -d "$XDG_STATE_HOME/bkpr/sessions" ]] &&
            [[ -n $(find "$XDG_STATE_HOME/bkpr/sessions" -mindepth 1 -maxdepth 1 -type d -print -quit) ]]; then
            echo "To run a harness in existing sandbox '$name', open it and run 'bkpr run' in a pane. Use a fresh sandbox name to launch directly." >&2
            exit 2
        fi
        set -- tui -- "$bin" "$@"
    fi
    exec "$bin" "$@"

# Stop an isolated sandbox's daemon and all its sessions, then delete its directory.
sandbox-stop name="default":
    #!/usr/bin/env bash
    set -euo pipefail
    dir=$(just sandbox-dir "$1")
    legacy="${XDG_RUNTIME_DIR:-/tmp}/bks-$1"
    socket="$dir/bkpr/d"
    if [[ ! -e "$dir" && -d "$legacy" ]]; then
        dir="$legacy"
        socket="$dir/c.sock"
    fi
    if [[ ! -e "$dir" ]]; then echo "no sandbox '$1'" >&2; exit 0; fi
    if [[ ! -d "$dir" || -L "$dir" || ! -O "$dir" ]]; then
        echo "Sandbox directory must be owned by you and not a symlink: $dir" >&2
        exit 2
    fi
    if [[ -x "$dir/bin/bkpr" ]] && "$dir/bin/bkpr" daemon --socket "$socket" info >/dev/null 2>&1; then
        "$dir/bin/bkpr" daemon --socket "$socket" stop
    fi
    rm -rf "$dir"

# Stop and delete all development sandboxes in the current runtime directory.
sandbox-clean:
    bash scripts/clean-sandboxes.sh

# Print a sandbox's private directory, rejecting names that are unsafe or too long.
[private]
sandbox-dir name:
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in *[!A-Za-z0-9_-]*|"") echo "sandbox name must be [A-Za-z0-9_-]+" >&2; exit 2 ;; esac
    if (( ${#1} > 16 )); then echo "sandbox name must be at most 16 characters" >&2; exit 2; fi
    dir="${XDG_RUNTIME_DIR:-$(cd /tmp && pwd -P)}/b-$1"
    # Match the native limit, including the named session's .agent companion.
    if (( ${#dir} + 78 >= 104 )); then
        echo "sandbox path $dir is too long for session sockets; set a shorter XDG_RUNTIME_DIR" >&2
        exit 2
    fi
    printf '%s\n' "$dir"

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
