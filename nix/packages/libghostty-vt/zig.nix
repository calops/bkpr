# The pinned Ghostty ABI requires Zig 0.17; nixpkgs b1b8759 only provides
# Zig through 0.16. Prefer nixpkgs' zig_0_17 as soon as it is available.
{
  lib,
  stdenvNoCC,
  fetchurl,
}:
let
  platforms = {
    x86_64-linux = {
      platform = "x86_64-linux";
      sha256 = "1cbe9df9f27e6b78d14ccbca43b6703a404ef79ef1c463de901d7f088d4e2026";
    };
    aarch64-linux = {
      platform = "aarch64-linux";
      sha256 = "9e8d11661d4ae3bd57702a3832781e23ad151dde5798e16a5ccd503f65234ff8";
    };
    aarch64-darwin = {
      platform = "aarch64-macos";
      sha256 = "b607e9b9234790a008116ae5bdb71c6243b84b9fb42a53a9e70fde41c06c536a";
    };
  };
  host = platforms.${stdenvNoCC.hostPlatform.system};
in
stdenvNoCC.mkDerivation (finalAttrs: {
  pname = "zig";
  version = "0.17.0";
  src = fetchurl {
    url = "https://ziglang.org/download/${finalAttrs.version}/zig-${host.platform}-${finalAttrs.version}.tar.xz";
    sha256 = host.sha256;
  };
  dontConfigure = true;
  dontBuild = true;
  dontFixup = true;
  installPhase = ''
    runHook preInstall
    mkdir -p "$out/bin"
    cp -R lib LICENSE "$out/"
    cp zig "$out/zig"
    # Preserve the official executable/lib layout, including when copied.
    ln -s ../zig "$out/bin/zig"
    runHook postInstall
  '';
  meta = {
    description = "Zig compiler required by the pinned libghostty-vt ABI";
    homepage = "https://ziglang.org";
    license = lib.licenses.mit;
    mainProgram = "zig";
    platforms = builtins.attrNames platforms;
    sourceProvenance = [ lib.sourceTypes.binaryNativeCode ];
  };
})
