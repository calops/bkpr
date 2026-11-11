{
  lib,
  stdenvNoCC,
  rustPlatform,
  fetchFromGitHub,
  makeWrapper,
  installShellFiles,
  ncurses,
  git,
  gh,
  openssh,
  libghostty-vt,
}:
stdenvNoCC.mkDerivation (
  finalAttrs:
  let
    rustc = rustPlatform.rust.rustc;
    unwrappedRustc = rustc.unwrapped or rustc;
    buildOnlyDependencies = [
      rustc
      unwrappedRustc
      rustPlatform.rust.cargo
      libghostty-vt.zig
    ];
    terminfoGenerator = builtins.toFile "bkpr-terminfo.zig" ''
      const std = @import("std");
      const terminfo = @import("terminfo/main.zig");
      pub fn main(init: std.process.Init) !void {
          var entry = terminfo.ghostty;
          entry.names = &.{ "bkpr", "Bkpr terminal multiplexer" };
          var capabilities: [terminfo.ghostty.capabilities.len]terminfo.Source.Capability = undefined;
          @memcpy(&capabilities, terminfo.ghostty.capabilities);
          // Reply regexes must match DA2/XTVERSION after tic unescapes them.
          for (&capabilities) |*capability| {
              if (std.mem.eql(u8, capability.name, "rv"))
                  capability.value = .{ .string = "\\E\\\\[>[0-9]+;[0-9]+;[0-9]+c" };
              if (std.mem.eql(u8, capability.name, "xr"))
                  capability.value = .{ .string = "\\EP>\\\\|[ -~]+\\E\\\\\\\\" };
          }
          entry.capabilities = &capabilities;
          var buffer: [16384]u8 = undefined;
          var stdout = std.Io.File.stdout().writer(init.io, &buffer);
          try entry.encode(&stdout.interface);
          try stdout.interface.flush();
      }
    '';
    terminfo = stdenvNoCC.mkDerivation {
      pname = "bkpr-terminfo";
      version = libghostty-vt.revision;
      inherit (libghostty-vt) src;
      nativeBuildInputs = [
        ncurses
        libghostty-vt.zig
      ];
      dontConfigure = true;
      buildPhase = ''
        runHook preBuild
        export ZIG_GLOBAL_CACHE_DIR="$TMPDIR/zig-cache"
        cp ${terminfoGenerator} src/bkpr-terminfo.zig
        printf '%s\n' '# Ghostty ${libghostty-vt.revision}, src/terminfo/ghostty.zig; Bkpr identity and reply patterns.' > bkpr.terminfo
        zig run -j8 src/bkpr-terminfo.zig >> bkpr.terminfo
        runHook postBuild
      '';
      installPhase = ''
        runHook preInstall
        mkdir -p "$out/share/terminfo"
        tic -x -o "$out/share/terminfo" bkpr.terminfo
        install -Dm444 bkpr.terminfo "$out/share/bkpr/terminfo/bkpr.terminfo"
        install -Dm444 LICENSE "$out/share/licenses/bkpr-terminfo/LICENSE"
        infocmp -x -A "$out/share/terminfo" bkpr > /dev/null
        runHook postInstall
      '';
    };
    binaries = rustPlatform.buildRustPackage {
      pname = "bkpr-binaries";
      inherit (finalAttrs)
        src
        version
        cargoHash
        cargoDeps
        ;
      nativeBuildInputs = [ installShellFiles ];
      BKPR_GHOSTTY_VT_DIR = libghostty-vt;
      GHOSTTY_INSTALL_DIR = libghostty-vt;
      # Precompiled std panic/source paths must not retain a Rust compiler in
      # the runtime closure. Remap both the nixpkgs wrapper and its compiler.
      RUSTFLAGS = lib.concatStringsSep " " [
        "--remap-path-prefix=${rustc}=/rust-toolchain"
        "--remap-path-prefix=${unwrappedRustc}=/rust-toolchain"
        "--remap-path-prefix=${finalAttrs.src}=/bkpr-source"
        "--remap-path-prefix=${libghostty-vt}=/ghostty-sdk"
      ];
      disallowedReferences = buildOnlyDependencies;
      cargoBuildFlags = [
        "-p"
        "bkpr"
        "-p"
        "bkpr-server"
        "--bins"
      ];
      preBuild = ''
        if [ "$NIX_BUILD_CORES" -gt 8 ] || [ "$NIX_BUILD_CORES" -le 0 ]; then
          export NIX_BUILD_CORES=8
        fi
        export CARGO_BUILD_JOBS="$NIX_BUILD_CORES"
      '';
      # Process/PTY acceptance needs an actual host user and terminal, rather
      # than the isolated Nix builder. Run it in coordinated release checks.
      doCheck = false;
      postInstall =
        ''
          test -x "$out/bin/bkpr"
          test -x "$out/bin/bkprd"
          install -Dm444 LICENSE "$out/share/licenses/bkpr/LICENSE"
          install -Dm444 vendor/libghostty-rs/LICENSE "$out/share/licenses/libghostty-rs/LICENSE"
          cp -R ${libghostty-vt}/share/licenses/libghostty-vt "$out/share/licenses/"
        ''
        + lib.optionalString (stdenvNoCC.buildPlatform.canExecute stdenvNoCC.hostPlatform) ''
          "$out/bin/bkpr" completions bash > "$TMPDIR/bkpr"
          "$out/bin/bkpr" completions zsh > "$TMPDIR/_bkpr"
          "$out/bin/bkpr" completions fish > "$TMPDIR/bkpr.fish"
          installShellCompletion --bash "$TMPDIR/bkpr" \
            --zsh "$TMPDIR/_bkpr" --fish "$TMPDIR/bkpr.fish"
        '';
      meta = finalAttrs.meta;
    };
  in
  {
    pname = "bkpr";
    version = "0.5.1-unstable";
    src = fetchFromGitHub {
      owner = "calops";
      repo = "bkpr";
      rev = "9a4280014c406fff49f2094b52c3b2cc8732a51c";
      hash = "sha256-aMQq/HF8ca/ppPn6MtrkVIA5I6v50A+NphtTXCrrED0=";
    };
    cargoHash = "sha256-Tc6Hjqh0M+8F11nRt1I22OwQnrHP56sL2U/9YtRHMaw=";
    # Local development can override src/version and supply importCargoLock
    # here without changing the independently pinned published source above.
    cargoDeps = null;
    nativeBuildInputs = [ makeWrapper ];
    dontUnpack = true;
    dontConfigure = true;
    dontBuild = true;
    disallowedReferences = buildOnlyDependencies;
    installPhase = ''
      runHook preInstall
      mkdir -p "$out/bin"
      # Keep both authenticated native executables adjacent under their real
      # names: current_exe() finds the matching daemon beside the actual CLI.
      for program in bkpr bkprd; do
        makeWrapper ${binaries}/bin/"$program" "$out/bin/$program" \
          --suffix PATH : ${
            lib.makeBinPath [
              git
              gh
              openssh
            ]
          } \
          --set BKPR_TERMINFO_DIR "${terminfo}/share/terminfo"
      done
      mkdir -p "$out/share"
      cp -rs ${binaries}/share/. "$out/share/"
      chmod u+w "$out/share/licenses"
      cp -rs ${terminfo}/share/. "$out/share/"
      runHook postInstall
    '';
    passthru = { inherit binaries terminfo; };
    meta = {
      description = "Pane-first native terminal multiplexer with persistent named workspaces";
      homepage = "https://github.com/calops/bkpr";
      changelog = "https://github.com/calops/bkpr/releases";
      license = lib.licenses.mit;
      mainProgram = "bkpr";
      platforms = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    };
  }
)
