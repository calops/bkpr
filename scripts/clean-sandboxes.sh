#!/usr/bin/env bash
set -euo pipefail

runtime=${XDG_RUNTIME_DIR:-$(cd /tmp && pwd -P)}
failed=0

clean() {
    local dir=$1 socket=$2
    local bin=$dir/bin/bkpr daemon=$dir/bin/bkprd
    if [[ -L $dir/bkpr || -L $socket ]]; then
        printf 'Preserved %s: unsafe socket path\n' "$dir" >&2
        return 1
    fi
    if [[ -S $socket ]]; then
        if [[ ! -x $bin ]] || ! "$bin" daemon --socket "$socket" stop; then
            printf 'Preserved %s: could not stop its daemon\n' "$dir" >&2
            return 1
        fi
    fi
    local processes
    processes=$(ps -u "$(id -u)" -o args=) || return 1
    if [[ -e $socket || $processes == "$daemon"* || $processes == *$'\n'"$daemon"* ]]; then
        printf 'Preserved %s: daemon has not exited\n' "$dir" >&2
        return 1
    fi
    rm -rf -- "$dir" || return 1
    printf 'Removed %s\n' "$dir"
}

# Reclaim space from inactive sandboxes before asking live ones to flush state.
for pass in inactive active; do
    for dir in "$runtime"/b-* "$runtime"/bks-*; do
        [[ -d $dir && ! -L $dir && -O $dir && ! -L $dir/bin ]] || continue
        name=${dir##*/}
        case $name in
            bks-*) name=${name#bks-}; socket=$dir/c.sock ;;
            b-*) name=${name#b-}; socket=$dir/bkpr/d ;;
        esac
        [[ $name =~ ^[A-Za-z0-9_-]{1,16}$ ]] || continue
        # A failed binary copy can leave just one member of the pair.
        [[ -f $dir/bin/bkpr || -f $dir/bin/bkprd ]] || continue
        [[ ! -L $dir/bin/bkpr && ! -L $dir/bin/bkprd ]] || continue
        if [[ -S $socket ]]; then
            [[ $pass == active ]] || continue
        else
            [[ $pass == inactive ]] || continue
        fi
        clean "$dir" "$socket" || failed=1
    done
done
exit "$failed"
