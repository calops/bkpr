{
  lib,
  stdenv,
  stdenvNoCC,
  fetchurl,
  callPackage,
  git,
  pkg-config,
  darwin,
  xcbuild,
}:
let
  revision = "93967b7f9478e94dcbb42125c8a90f34feb40d75";
  sourceSha256 = "af551aa496cf54ff734e11563d45fe2c49739ee754d1740d1ac984a67a42dc70";
  patchSha256 = "196b39997c6552d9158f763555a72784aae2dbf4fa61aa500554bfd07e28d32c";
  platforms = {
    x86_64-linux = {
      platform = "x86_64-linux";
      target = "x86_64-linux-gnu";
      sha256 = "1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026";
    };
    aarch64-linux = {
      platform = "aarch64-linux";
      target = "aarch64-linux-gnu";
      sha256 = "9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8";
    };
    aarch64-darwin = {
      platform = "aarch64-macos";
      target = "aarch64-macos";
      sha256 = "b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a";
    };
  };
  host = platforms.${stdenv.hostPlatform.system};
  zig = stdenvNoCC.mkDerivation {
    pname = "bkpr-zig";
    version = "0.17.0";
    src = fetchurl {
      url = "https://ziglang.org/download/0.17.0/zig-${host.platform}-0.17.0.tar.xz";
      inherit (host) sha256;
    };
    dontConfigure = true;
    dontBuild = true;
    dontFixup = true;
    installPhase = ''
      mkdir -p "$out/bin"
      cp -R lib LICENSE "$out/"
      cp zig "$out/zig"
      # Retain the official executable/lib layout; PATH uses a convenience link.
      ln -s ../zig "$out/bin/zig"
    '';
    meta.license = lib.licenses.mit;
  };
  # Vendored from this exact Ghostty revision, not imported from a fetched
  # derivation at evaluation time. All transitive sources have fixed hashes.
  dependencies = callPackage ./ghostty-dependencies.nix { zig_0_17 = zig; };
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
  patches = [ ../vendor/libghostty-rs/crates/libghostty-vt-sys/native/clipboard-offer.patch ];
  patchFlags = [
    "-p1"
    "--fuzz=0"
  ];
  prePatch = ''
    printf '%s  %s\n' '${patchSha256}' '${../vendor/libghostty-rs/crates/libghostty-vt-sys/native/clipboard-offer.patch}' | sha256sum --check --status
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
      -Demit-lib-vt=true -Doptimize=ReleaseSafe -Dsimd=true -Dcpu=baseline -Dtarget=${host.target} \
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
    platforms = builtins.attrNames platforms;
  };
}
