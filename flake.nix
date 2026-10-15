{
  description = "shephrd — workspace-first native Zellij terminals and sidebar";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "x86_64-darwin"
        "aarch64-darwin"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
      pkgsFor =
        system:
        import nixpkgs {
          inherit system;
          overlays = [ rust-overlay.overlays.default ];
        };
      toolchainFor = pkgs: pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
      zellijFor =
        pkgs:
        pkgs.zellij.override {
          zellij-unwrapped = pkgs.zellij-unwrapped.overrideAttrs (old: {
            patches = (old.patches or [ ]) ++ [
              ./nix/zellij-cursor.patch
              ./nix/zellij-program-status.patch
              ./nix/zellij-program-status-events.patch
              ./nix/zellij-host-replies.patch
              ./nix/zellij-cli-session.patch
              ./nix/zellij-popup-clicks.patch
              ./nix/zellij-overlay-geometry.patch
              ./nix/zellij-plugin-messages.patch
            ];
            doCheck = true;
            cargoTestFlags = [
              "--package"
              "zellij-server"
              "--lib"
              "program_status"
            ];
            postCheck = (old.postCheck or "") + ''
              cargo test --offline --package zellij-client --lib stdin_ansi_parser
              cargo test --offline --package zellij-client --lib stdin_handler
              cargo test --offline --package zellij-server --lib pinned_
              cargo test --offline --package zellij-server --lib passive_popup
              cargo test --offline --package zellij-server --lib viewport_geometry_regressions
              cargo test --offline --package zellij-server --lib test_ctrl_drag_resizes
              cargo test --offline --package zellij-server --lib render_messages_tests -- --test-threads=1
              cargo test --offline --package zellij-server --lib screen_tab_switch_
              cargo test --offline --package zellij-server --lib screen_plugin_
            '';
          });
        };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          toolchain = toolchainFor pkgs;
          zellij = zellijFor pkgs;
          rustPlatform = pkgs.makeRustPlatform {
            cargo = toolchain;
            rustc = toolchain;
          };
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./LICENSE
              ./crates
            ];
          };
          native = rustPlatform.buildRustPackage {
            pname = "shephrd-native";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFileContents = builtins.readFile ./Cargo.lock;
            nativeCheckInputs = [ pkgs.git ];
            cargoBuildFlags = [
              "-p"
              "shephrd"
              "--bin"
              "shephrd"
            ];
            cargoTestFlags = [
              "-p"
              "shephrd"
            ];
            postInstall = ''
              install -Dm444 LICENSE "$out/share/licenses/shephrd/LICENSE"
            '';
            meta.license = pkgs.lib.licenses.mit;
            meta.mainProgram = "shephrd";
          };
          plugin = rustPlatform.buildRustPackage {
            pname = "shephrd-zellij";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFileContents = builtins.readFile ./Cargo.lock;
            buildPhase = ''
              runHook preBuild
              cargo build --release --locked --offline -p shephrd-zellij --target wasm32-wasip1
              runHook postBuild
            '';
            doCheck = false;
            installPhase = ''
              runHook preInstall
              install -Dm444 target/wasm32-wasip1/release/shephrd-zellij.wasm \
                "$out/share/shephrd/shephrd-zellij.wasm"
              install -Dm444 LICENSE "$out/share/licenses/shephrd/LICENSE"
              runHook postInstall
            '';
            meta.license = pkgs.lib.licenses.mit;
          };
          product =
            pkgs.runCommand "shephrd-0.1.0"
              {
                nativeBuildInputs = [ pkgs.makeWrapper ];
                meta.mainProgram = "shephrd";
                meta.license = pkgs.lib.licenses.mit;
              }
              ''
                mkdir -p "$out/bin" "$out/share/shephrd"
                ln -s ${plugin}/share/shephrd/shephrd-zellij.wasm "$out/share/shephrd/"
                mkdir -p "$out/share/licenses/shephrd"
                ln -s ${native}/share/licenses/shephrd/LICENSE "$out/share/licenses/shephrd/LICENSE"
                makeWrapper ${native}/bin/shephrd "$out/bin/shephrd" \
                  --set SHEPHRD_PLUGIN ${plugin}/share/shephrd/shephrd-zellij.wasm \
                  --set SHEPHRD_ZELLIJ ${zellij}/bin/zellij \
                  --prefix PATH : ${
                    pkgs.lib.makeBinPath [
                      pkgs.git
                      pkgs.gh
                    ]
                  }
              '';
        in
        {
          default = product;
          shephrd = product;
          shephrd-native = native;
          shephrd-zellij = plugin;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.shephrd}/bin/shephrd";
          meta.description = "Launch shephrd with native Zellij terminals and its bundled sidebar";
        };
        shephrd = self.apps.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          zellij = pkgs.zellij;
        in
        {
          default = pkgs.mkShell {
            packages = [
              (toolchainFor pkgs)
              zellij
              pkgs.git
              pkgs.gh
              pkgs.pkg-config
              (pkgs.python3.withPackages (ps: [ ps.pyte ]))
            ];
            buildInputs = [
              pkgs.openssl
              pkgs.curl
            ];
            SHEPHRD_ZELLIJ = "${zellij}/bin/zellij";
            SHEPHRD_ZELLIJ_VENDOR = pkgs.zellij-unwrapped.cargoDeps;
            shellHook = ''
              export SHEPHRD_PLUGIN="$PWD/target/wasm32-wasip1/debug/shephrd-zellij.wasm"
              if [ -x "$PWD/target/zellij-dev/target/debug/zellij" ]; then
                export SHEPHRD_ZELLIJ="$PWD/target/zellij-dev/target/debug/zellij"
              fi
            '';
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
