{
  description = "shephrd — workspace-first Herdr composition in Zellij";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    herdr = {
      url = "github:herdrdev/herdr/v0.9.3";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      herdr,
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
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          toolchain = toolchainFor pkgs;
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
            pname = "shephrd-companion";
            version = "0.1.0";
            inherit src;
            cargoLock.lockFileContents = builtins.readFile ./Cargo.lock;
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
                  --set SHEPHRD_HERDR ${herdr.packages.${system}.default}/bin/herdr \
                  --set SHEPHRD_ZELLIJ ${pkgs.zellij}/bin/zellij \
                  --set SHEPHRD_SSH ${pkgs.openssh}/bin/ssh \
                  --prefix PATH : ${
                    pkgs.lib.makeBinPath [
                      pkgs.git
                      pkgs.openssh
                    ]
                  }
              '';
        in
        {
          default = product;
          shephrd = product;
          shephrd-companion = native;
          shephrd-zellij = plugin;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.shephrd}/bin/shephrd";
          meta.description = "Launch shephrd with its bundled Zellij frontend and upstream Herdr";
        };
        shephrd = self.apps.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              (toolchainFor pkgs)
              herdr.packages.${system}.default
              pkgs.zellij
              pkgs.openssh
              pkgs.git
              pkgs.pkg-config
            ];
            SHEPHRD_HERDR = "${herdr.packages.${system}.default}/bin/herdr";
            SHEPHRD_ZELLIJ = "${pkgs.zellij}/bin/zellij";
            SHEPHRD_SSH = "${pkgs.openssh}/bin/ssh";
            shellHook = ''
              export SHEPHRD_PLUGIN="$PWD/target/wasm32-wasip1/release/shephrd-zellij.wasm"
            '';
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
