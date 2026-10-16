#!/usr/bin/env bash
set -euo pipefail

mode=$1
tag=$2
case "$mode" in
  build)
    if [[ $tag != unstable ]]; then
      echo 'build=true'
      exit 0
    fi
    # A delayed push event must not rebuild a label that has already moved.
    current=$(git ls-remote origin refs/tags/unstable | cut -f1)
    if [[ -n $current && $current != "$(git rev-parse HEAD)" ]]; then
      echo 'build=false'
      exit 0
    fi
    # Another tag on this commit owns the build; the moving label needs none.
    for candidate in $(git tag --points-at HEAD); do
      if [[ $candidate != unstable ]]; then
        echo 'build=false'
        exit 0
      fi
    done
    echo 'build=true'
    ;;
  promote)
    [[ $tag != unstable ]] || exit 0
    # Refresh the moving label before comparing ancestry; never move it backwards
    # or across divergent histories. A lease protects against concurrent promotions.
    old=$(git ls-remote origin refs/tags/unstable | cut -f1)
    if [[ -n $old ]]; then
      git fetch --no-tags origin refs/tags/unstable
      if [[ $old == "$(git rev-parse HEAD)" ]] || ! git merge-base --is-ancestor "$old" HEAD; then
        exit 0
      fi
    fi
    git push --force-with-lease="refs/tags/unstable:$old" origin "HEAD:refs/tags/unstable"
    ;;
  *) echo "unknown tag policy: $mode" >&2; exit 1 ;;
esac
