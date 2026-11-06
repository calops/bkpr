{
  lib,
  makeRustPlatform,
  runCommand,
  makeWrapper,
  git,
  gh,
  openssh,
  toolchain,
  libghostty-vt,
  src,
  version,
}:
let
  rustPlatform = makeRustPlatform {
    cargo = toolchain;
    rustc = toolchain;
  };
  binaries = rustPlatform.buildRustPackage {
    pname = "bkpr-binaries";
    inherit src version;
    cargoLock.lockFileContents = builtins.readFile ../Cargo.lock;
    BKPR_GHOSTTY_VT_DIR = libghostty-vt;
    GHOSTTY_INSTALL_DIR = libghostty-vt;
    # Panic/source filenames in precompiled std previously retained the entire
    # Rust compiler in the runtime closure. Keep that invariant enforced here.
    RUSTFLAGS = lib.concatStringsSep " " [
      "--remap-path-prefix=${toolchain}=/rust-toolchain"
      "--remap-path-prefix=${src}=/bkpr-source"
      "--remap-path-prefix=${libghostty-vt}=/ghostty-sdk"
    ];
    disallowedReferences = [
      toolchain
      libghostty-vt.zig
    ];
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
    # Native process/PTY acceptance runs in the coordinated release checks, not
    # in an isolated Nix builder with no terminal or authenticated live daemon.
    doCheck = false;
    postInstall = ''
      test -x "$out/bin/bkpr"
      test -x "$out/bin/bkprd"
      install -Dm444 LICENSE "$out/share/licenses/bkpr/LICENSE"
      install -Dm444 vendor/libghostty-rs/LICENSE "$out/share/licenses/libghostty-rs/LICENSE"
      cp -R ${libghostty-vt}/share/licenses/libghostty-vt "$out/share/licenses/"
    '';
    meta = {
      license = lib.licenses.mit;
      mainProgram = "bkpr";
    };
  };
in
# Keep the actual executables adjacent and under their original filenames:
# current_exe() locates bkprd beside bkpr and authenticates native executables.
# Outer launchers supply optional system tools without renaming either binary.
runCommand "bkpr-${version}"
  {
    nativeBuildInputs = [ makeWrapper ];
    disallowedReferences = [
      toolchain
      libghostty-vt.zig
    ];
    passthru = { inherit binaries; };
    meta = {
      description = "Pane-first native terminal multiplexer with persistent named workspaces";
      homepage = "https://github.com/calops/bkpr";
      license = lib.licenses.mit;
      mainProgram = "bkpr";
      platforms = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];
    };
  }
  ''
    mkdir -p "$out/bin"
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
  ''
