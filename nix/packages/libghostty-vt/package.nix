{
  lib,
  stdenv,
  fetchurl,
  callPackage,
  git,
  pkg-config,
  darwin,
  xcbuild,
  zig_0_17 ? null,
}:
let
  revision = "93967b7f9478e94dcbb42125c8a90f34feb40d75";
  sourceSha256 = "af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70";
  patchSha256 = "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c";
  targets = {
    x86_64-linux = "x86_64-linux-gnu";
    aarch64-linux = "aarch64-linux-gnu";
    aarch64-darwin = "aarch64-macos";
  };
  target = targets.${stdenv.hostPlatform.system};
  # Use the ordinary nixpkgs compiler when available. The pinned nixpkgs
  # snapshot only has Zig <= 0.16, which cannot build this native revision.
  zig = if zig_0_17 != null then zig_0_17 else callPackage ./zig.nix { };
  # Vendored zon2nix output from this exact Ghostty revision. Do not import
  # it from a fetched derivation: every transitive source is fixed-hash.
  dependencies = callPackage ./dependencies.nix { zig_0_17 = zig; };
  # This contribution is not merged upstream. Fetch the exact reviewed patch
  # from Bkpr's immutable release, independently of Bkpr's local source tree.
  clipboardOfferPatch = fetchurl {
    url = "https://raw.githubusercontent.com/calops/bkpr/v0.5.0/vendor/libghostty-rs/crates/libghostty-vt-sys/native/clipboard-offer.patch";
    sha256 = patchSha256;
  };
  simdutfLicense = fetchurl {
    url = "https://raw.githubusercontent.com/simdutf/simdutf/v9.0.0/LICENSE-MIT";
    sha256 = "fc8dbc04e03ad4efc08a647ffe7f995b811a95bc04c0e85a56d5277c6593fa5f";
  };
in
stdenv.mkDerivation {
  pname = "libghostty-vt";
  version = "0-93967b7-offer-v1";
  src = fetchurl {
    name = "ghostty-${revision}.tar.gz";
    url = "https://codeload.github.com/ghostty-org/ghostty/tar.gz/${revision}";
    sha256 = sourceSha256;
  };
  patches = [ clipboardOfferPatch ];
  patchFlags = [
    "-p1"
    "--fuzz=0"
  ];
  prePatch = ''
    printf '%s  %s\n' '${patchSha256}' '${clipboardOfferPatch}' | sha256sum --check --status
  '';
  nativeBuildInputs = [
    git
    pkg-config
  ]
  ++ lib.optionals stdenv.hostPlatform.isDarwin [
    darwin.cctools
    xcbuild
  ];
  dontConfigure = true;
  # Hash the final archive after Nix fixups, not an archive later stripped by a
  # generic hook. Preserve the preparation script's ReleaseSafe build profile.
  dontStrip = true;
  disallowedReferences = [
    zig
    dependencies
  ];
  buildPhase = ''
    runHook preBuild
    export HOME="$TMPDIR/home"
    export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
    mkdir -p "$HOME" "$ZIG_GLOBAL_CACHE_DIR" "$TMPDIR/zig"
    # Zig panic/source locations must not embed a store toolchain or package
    # tree. Use real build-local copies, also avoiding Zig's symlink bugs.
    cp -R ${zig}/. "$TMPDIR/zig/"
    cp -R ${dependencies} zig-packages
    chmod -R u+w "$TMPDIR/zig" zig-packages
    "$TMPDIR/zig/bin/zig" build -j8 --system "$PWD/zig-packages" \
      -Demit-lib-vt=true -Doptimize=ReleaseSafe -Dsimd=true -Dcpu=baseline -Dtarget=${target} \
      -Dapp-runtime=none -Demit-xcframework=false \
      --prefix "$TMPDIR/ghostty-install"
    runHook postBuild
  '';
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/lib" "$out/share/licenses/libghostty-vt"
    cp "$TMPDIR/ghostty-install/lib/libghostty-vt.a" "$out/lib/"
    cp -R "$TMPDIR/ghostty-install/include" "$out/"
    install -m444 LICENSE "$out/share/licenses/libghostty-vt/LICENSE"
    install -m444 ${simdutfLicense} "$out/share/licenses/libghostty-vt/simdutf-MIT"
    # Retain the bundled source's additional third-party copyright notices.
    install -m444 pkg/simdutf/vendor/simdutf.h "$out/share/licenses/libghostty-vt/simdutf-notices.h"
    find zig-packages -maxdepth 2 -type f \
      \( -iname 'LICENSE*' -o -iname 'COPYING*' -o -name 'OFL.txt' \) \
      -exec cp --parents '{}' "$out/share/licenses/libghostty-vt/" \;
    printf '%s\n' '${revision}' > "$out/bkpr-source-revision"
    printf '%s\n' '${revision}' > "$out/ghostty-source-revision"
    printf '%s\n' '${sourceSha256}' > "$out/bkpr-source-sha256"
    printf '%s\n' '${patchSha256}' > "$out/bkpr-source-patch-sha256"
    printf '%s\n' 'ReleaseSafe-simd-libc-baseline-offer-v1' > "$out/bkpr-build-profile"
    runHook postInstall
  '';
  postFixup = ''
    (cd "$out" && find include -type f -print0 | sort -z | xargs -0 sha256sum; \
      cd "$out" && sha256sum lib/libghostty-vt.a) > "$out/bkpr-files.sha256"
  '';
  passthru = { inherit zig dependencies revision; };
  meta = {
    description = "Pinned Ghostty VT static SDK with Bkpr paste-offer correlation";
    homepage = "https://ghostty.org";
    license = with lib.licenses; [
      mit
      asl20
      bsd3
    ];
    platforms = builtins.attrNames targets;
  };
}
