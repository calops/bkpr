{
  description = "bkpr — pane-first native terminal multiplexer";

  nixConfig = {
    extra-substituters = [
      "https://bkpr.cachix.org"
      "https://cache.numtide.com"
    ];
    extra-trusted-public-keys = [
      "bkpr.cachix.org-1:uAJ+wudjeXgYbimGGreY8LgWfk0j9JaxNBjDxQ0h18w="
      "niks3.numtide.com-1:DTx8wZduET09hRmMtKdQDxNNthLQETkc/yaX7M4qK0g="
    ];
  };

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # Keep the distribution's own dependency pins for binary-cache reuse.
    llm-agents.url = "github:numtide/llm-agents.nix/90011236663d4116738fc2d63de6ab64523387b6";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      llm-agents,
      ...
    }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
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
      version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
      src = nixpkgs.lib.fileset.toSource {
        root = ./.;
        fileset = nixpkgs.lib.fileset.unions [
          ./Cargo.toml
          ./Cargo.lock
          ./LICENSE
          ./crates
          ./vendor/libghostty-rs
          ./integration
        ];
      };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libghostty-vt = pkgs.callPackage ./nix/packages/libghostty-vt/package.nix { };
          bkpr =
            (pkgs.callPackage ./nix/packages/bkpr/package.nix {
              inherit libghostty-vt;
            }).overrideAttrs
              {
                inherit src version;
                # The standalone package defaults to the published release and its
                # cargoHash. Development/release CI uses this workspace's lock.
                cargoDeps = pkgs.rustPlatform.importCargoLock {
                  lockFile = ./Cargo.lock;
                };
              };
        in
        {
          inherit bkpr libghostty-vt;
          default = bkpr;
        }
      );

      apps = forAllSystems (system: {
        default = {
          type = "app";
          program = "${self.packages.${system}.bkpr}/bin/bkpr";
          meta.description = "Launch the pane-first native terminal multiplexer";
        };
        bkpr = self.apps.${system}.default;
      });

      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          agents = llm-agents.packages.${system};
          harnessPackages = [
            agents.claude-code
            agents.codex
            agents.omp
            agents.pi
            agents.antigravity-cli
            agents.cursor-agent
            agents.copilot-cli
            pkgs.aider-chat
            agents.cline
            agents.goose-cli
          ];
          # The distribution names v2's executable opencode2 for coexistence.
          # Bkpr resolves the native canonical name selected by this shell.
          opencode2 = pkgs.runCommand "opencode2-canonical-${agents.opencode2.version}" { } ''
            mkdir -p "$out/bin"
            ln -s ${agents.opencode2}/bin/opencode2 "$out/bin/opencode"
          '';
        in
        {
          default = pkgs.mkShell {
            name = "bkpr";
            packages = [
              (toolchainFor pkgs)
              pkgs.just
              pkgs.git
              pkgs.gh
              pkgs.nodejs
              pkgs.pkg-config
              pkgs.curl
              pkgs.patch
              pkgs.gnutar
              pkgs.xz
              pkgs.coreutils
              pkgs.findutils
              pkgs.imagemagick
              (pkgs.python3.withPackages (ps: [ ps.pyte ]))
            ]
            ++ pkgs.lib.optionals pkgs.stdenv.hostPlatform.isLinux [ pkgs.xdotool ];
          };
          harnesses = pkgs.mkShell {
            name = "bkpr-harnesses";
            inputsFrom = [ self.devShells.${system}.default ];
            packages = harnessPackages ++ [ opencode2 ];
          };
        }
        // pkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
          showcase = pkgs.mkShell {
            name = "bkpr-showcase";
            packages = [
              pkgs.just
              pkgs.git
              pkgs.bashInteractive
              pkgs.nodejs
              pkgs.kitty
              pkgs.xorg-server
              pkgs.xdotool
              pkgs.openssh
              pkgs.openssl
              pkgs.util-linux
              pkgs.iproute2
              pkgs.coreutils
              pkgs.imagemagick
              pkgs.fontconfig
              agents.claude-code
              agents.codex
              agents.pi
              (pkgs.python3.withPackages (ps: [
                ps.pillow
                ps.pyte
                ps.regex
                ps.wcwidth
              ]))
            ];
            FONTCONFIG_FILE = pkgs.makeFontsConf {
              fontDirectories = [
                pkgs.nerd-fonts.jetbrains-mono
                pkgs.noto-fonts-color-emoji
              ];
            };
            SHOWCASE_FONT = "JetBrainsMono Nerd Font";
            LIBGL_DRIVERS_PATH = "${pkgs.mesa}/lib/dri";
            __EGL_VENDOR_LIBRARY_FILENAMES = "${pkgs.mesa}/share/glvnd/egl_vendor.d/50_mesa.json";
            __GLX_VENDOR_LIBRARY_NAME = "mesa";
            LIBGL_ALWAYS_SOFTWARE = "1";
          };
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
