#!/usr/bin/env bash
set -euo pipefail

system=$1
tag=$2
host="$RUNNER_TEMP/zellij-bkpr"
case "$system" in
  x86_64-linux) target=x86_64-unknown-linux-musl ;;
  aarch64-linux) target=aarch64-unknown-linux-musl ;;
  aarch64-darwin) target=aarch64-apple-darwin ;;
  *) echo "unsupported system: $system" >&2; exit 1 ;;
esac

# Keep the native release independent of the Nix toolchain and store.
if [[ $(rustc --print sysroot) == /nix/store/* ]]; then
  echo 'portable build requires a non-Nix Rust toolchain' >&2
  exit 1
fi
rustup target add "$target" wasm32-wasip1

zellij_rev=$(jq -er '
  . as $lock
  | $lock.nodes[$lock.root].inputs["bkpr-zellij"] as $input
  | if ($input | type) != "string" then
      error("bkpr-zellij must be a direct locked input")
    else
      $lock.nodes[$input].locked
    end
  | select(.type == "github" and .owner == "calops" and .repo == "bkpr-zellij")
  | .rev
  | select(type == "string" and test("^[0-9a-f]{40}$"))
' "$GITHUB_WORKSPACE/flake.lock")
git init "$host"
git -C "$host" remote add origin https://github.com/calops/bkpr-zellij.git
git -C "$host" fetch --depth 1 origin "$zellij_rev"
git -C "$host" checkout --detach "$zellij_rev"

if [[ $system == *-linux ]]; then
  target_key=${target^^}
  target_key=${target_key//-/_}
  export "CC_${target//-/_}=musl-gcc"
  export "CARGO_TARGET_${target_key}_LINKER=musl-gcc"
  export OPENSSL_STATIC=1
  export CURL_STATIC=1
  zellij_features=(--features vendored_curl)
else
  zellij_features=()
fi

cargo build --locked --release --target "$target" --manifest-path "$host/Cargo.toml" --no-default-features "${zellij_features[@]}"
cargo build --locked --release --target "$target" -p bkpr --bin bkpr
cargo build --locked --release --target wasm32-wasip1 -p bkpr-zellij

bundle="$RUNNER_TEMP/bkpr-$tag-$system"
mkdir -p "$bundle"
install -m755 "target/$target/release/bkpr" "$bundle/bkpr"
install -m755 "$host/target/$target/release/zellij" "$bundle/zellij"
install -m644 target/wasm32-wasip1/release/bkpr-zellij.wasm "$bundle/bkpr-zellij.wasm"
if [[ $system == *-linux ]]; then
  for program in bkpr zellij; do
    if [[ $(readelf -l "$bundle/$program") == *INTERP* || $(readelf -d "$bundle/$program") == *NEEDED* ]]; then
      echo "$program is not statically linked" >&2
      exit 1
    fi
  done
else
  for program in bkpr zellij; do
    while IFS= read -r dependency; do
      case "$dependency" in
        /usr/lib/*|/System/Library/*) ;;
        *) echo "$program depends on non-system library: $dependency" >&2; exit 1 ;;
      esac
    done < <(otool -L "$bundle/$program" | awk 'NR > 1 { print $1 }')
  done
fi
"$bundle/zellij" --version | { read -r version; test "$version" = 'zellij 0.45.1-bkpr.1'; }
"$bundle/bkpr" --version
python3 .github/scripts/smoke-portable.py "$bundle"
# Exactly three runtime files; the archive is the single per-platform download.
tar -czf "bkpr-$tag-$system.tar.gz" -C "$RUNNER_TEMP" "bkpr-$tag-$system"
