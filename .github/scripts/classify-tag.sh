#!/usr/bin/env bash
set -eu

if [[ ${1:-} =~ ^v[1-9][0-9]*\. ]]; then
  echo 'Releases must remain in 0.x; refusing a nonzero major tag.' >&2
  exit 1
fi
pattern='^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-([0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*))?(\+[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?$'
if [[ ! "${1:-}" =~ $pattern ]]; then
  exit 0
fi

prerelease=${BASH_REMATCH[5]}
if [[ -n "$prerelease" ]]; then
  IFS='.' read -ra identifiers <<< "$prerelease"
  for identifier in "${identifiers[@]}"; do
    if [[ "$identifier" =~ ^0[0-9]+$ ]]; then
      exit 0
    fi
  done
fi

echo 'release=true'
if [[ -n "$prerelease" ]]; then
  echo 'prerelease=true'
else
  echo 'prerelease=false'
fi
