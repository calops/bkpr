{
  lib,
  stdenvNoCC,
  rustPlatform,
  fetchFromGitHub,
  makeWrapper,
  installShellFiles,
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
          }
      done
      ln -s ${binaries}/share "$out/share"
      runHook postInstall
    '';
    passthru = { inherit binaries; };
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
