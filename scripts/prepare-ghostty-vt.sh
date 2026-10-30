#!/usr/bin/env bash
# Isolated libghostty-vt prerequisite. No product/runtime configuration is changed.
set -euo pipefail
revision=93967b7f9478e94dcbb42125c8a90f34feb40d75
source_sha=af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70
root=${CARGO_TARGET_DIR:?Set CARGO_TARGET_DIR to the shared canonical target directory}
root=$(realpath "$root")
case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) platform=x86_64-linux; zig_sha=1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026 ;;
  Linux-aarch64) platform=aarch64-linux; zig_sha=9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8 ;;
  Darwin-arm64) platform=aarch64-macos; zig_sha=b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a ;;
  *) printf 'Unsupported prerequisite host\n' >&2; exit 1 ;;
esac
prefix="$root/ghostty-vt-93967b7"
if [[ ${1:-} == --print-prefix && $# == 1 ]]; then
  printf '%s\n' "$prefix"
  exit 0
fi
if [[ $# != 0 ]]; then printf 'Usage: %s [--print-prefix]\n' "$0" >&2; exit 1; fi
mkdir -p "$root"
fetch() {
  local url=$1 path=$2 sha=$3
  if [[ ! -f "$path" ]]; then curl --fail --location --retry 2 "$url" -o "$path"; fi
  printf '%s  %s\n' "$sha" "$path" | sha256sum --check --status
}
fetch "https://codeload.github.com/ghostty-org/ghostty/tar.gz/$revision" "$root/ghostty-93967b7.tar.gz" "$source_sha"
fetch "https://ziglang.org/download/0.17.0/zig-$platform-0.17.0.tar.xz" "$root/zig-0.17.0-$platform.tar.xz" "$zig_sha"
source_dir="$root/ghostty-$revision"
zig_dir="$root/zig-$platform-0.17.0"
# Restore every archive-owned file on each preparation: an edited source tree
# or toolchain under target must not be silently blessed as the pinned input.
tar -xzf "$root/ghostty-93967b7.tar.gz" -C "$root"
tar -xJf "$root/zig-0.17.0-$platform.tar.xz" -C "$root"
patch_path=$(realpath "$(dirname "${BASH_SOURCE[0]}")/../crates/bkpr-terminal/native/clipboard-offer.patch")
patch_sha=196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c
printf '%s  %s\n' "$patch_sha" "$patch_path" | sha256sum --check --status
patch --batch --fuzz=0 --forward -p1 -d "$source_dir" -i "$patch_path"
# The official SIMD build links libc. The no-libc build includes Wuffs' weak
# calloc/free stubs, which can preempt Rust's libc allocation calls when linked
# statically. Bundled SIMD dependencies use no-libcxx; baseline CPU avoids
# requiring this build machine's optional CPU instructions on another host.
(cd "$source_dir" && "$zig_dir/zig" build -j8 -Demit-lib-vt=true -Doptimize=ReleaseSafe -Dsimd=true -Dcpu=baseline --prefix "$prefix")
printf '%s\n' "$revision" > "$prefix/bkpr-source-revision"
printf '%s\n' "$source_sha" > "$prefix/bkpr-source-sha256"
printf '%s\n' 'ReleaseSafe-simd-libc-baseline-offer-v1' > "$prefix/bkpr-build-profile"
printf '%s\n' "$patch_sha" > "$prefix/bkpr-source-patch-sha256"
(cd "$prefix" && find include -type f -print0 | sort -z | xargs -0 sha256sum; cd "$prefix" && sha256sum lib/libghostty-vt.a) > "$prefix/bkpr-files.sha256"
printf 'export BKPR_GHOSTTY_VT_DIR=%q\n' "$prefix"
