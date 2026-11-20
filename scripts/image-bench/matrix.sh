#!/usr/bin/env bash
# Run the standard image-pipeline matrix for one binary pair.
# Usage: scripts/image-bench/matrix.sh <pair-dir> <label> [repeat]
# Topologies: local; bkpr remote mode over loopback; both remote forms over a
# shaped 50 Mbit/s, 20 ms one-way link (client on host + terminal over SSH = ssh-tty).
set -u
pair=$1 label=$2 repeat=${3:-1}
here=$(dirname "$0")
runs=target/image-bench/runs
mkdir -p "$runs"
for topology in local remote remote:50,20 ssh-tty:50,20; do
  name=${topology/:/-}
  name=${name/,20/}
  link=()
  if [[ $topology == *:* ]]; then link=(--link "${topology#*:}"); fi
  for spec in "png 12 2" "png 30 10" "strips 45 10"; do
    set -- $spec
    for attempt in $(seq 1 "$repeat"); do
      out="$runs/$label-$name-$1-${3}fps-$attempt"
      rm -rf "$out" "$out.log"
      if ! timeout 500 python3 "$here/run.py" --pair "$pair" --topology "${topology%%:*}" "${link[@]}" \
          --mode "$1" --frames "$2" --fps "$3" --out "$out" > "$out.log" 2>&1; then
        echo "FAILED $out"; tail -n 3 "$out.log"
      fi
    done
  done
done
