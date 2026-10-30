#!/usr/bin/env bash
# Reconstruct the checked-in upstream dependency without running a compiler.
# OUTPUT_DIRECTORY must not exist; this never overlays a developer checkout.
set -euo pipefail
if [[ $# != 1 ]]; then printf 'Usage: %s OUTPUT_DIRECTORY\n' "$0" >&2; exit 1; fi
revision=575ed6b8ceb79ffaf247ecf7e34333876b0d5be4
source_sha=fc16a84c73ec9783ce078e2c2d24a64b7e03e7d11039694f06a66e9865d2eeb8
patch_sha=408430a41477570f80ebf0b8e216f8150a6e2839ba88783df58d600c919002c6
repo=$(realpath "$(dirname "${BASH_SOURCE[0]}")/..")
cache=${CARGO_TARGET_DIR:?Set CARGO_TARGET_DIR to the shared canonical target directory}
mkdir -p "$cache"
cache=$(realpath "$cache")
output=$(realpath -m "$1")
if [[ -e "$output" ]]; then printf 'Refusing existing output: %s\n' "$output" >&2; exit 1; fi
archive="$cache/libghostty-rs-$revision.tar.gz"
if [[ ! -f "$archive" ]]; then
  curl --fail --location --retry 2 "https://codeload.github.com/Uzaaft/libghostty-rs/tar.gz/$revision" -o "$archive"
fi
printf '%s  %s\n' "$source_sha" "$archive" | sha256sum --check --status
patch_path="$repo/vendor/libghostty-rs/patches/sdk-native-and-offer.patch"
printf '%s  %s\n' "$patch_sha" "$patch_path" | sha256sum --check --status
scratch=$(mktemp -d "$cache/libghostty-sdk.XXXXXX")
trap 'rm -rf -- "$scratch"' EXIT
tar -xzf "$archive" -C "$scratch"
source_dir="$scratch/libghostty-rs-$revision"
patch --batch --fuzz=0 --forward -p1 -d "$source_dir" -i "$patch_path" < /dev/null
mkdir -p "$output/patches"
cp -a "$source_dir/crates" "$source_dir/Cargo.toml" "$source_dir/LICENSE" "$output/"
cp "$patch_path" "$output/patches/"
cp "$repo/vendor/libghostty-rs/UPSTREAM.toml" "$output/"
printf '%s\n' "$output"
