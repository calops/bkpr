{
  description = "bkpr — pane-first native terminal multiplexer";

  nixConfig = {
    extra-substituters = [ "https://bkpr.cachix.org" ];
    extra-trusted-public-keys = [ "bkpr.cachix.org-1:uAJ+wudjeXgYbimGGreY8LgWfk0j9JaxNBjDxQ0h18w=" ];
  };

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
          ./integration/omp
        ];
      };
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
          libghostty-vt = pkgs.callPackage ./nix/libghostty-vt.nix { };
          bkpr = pkgs.callPackage ./nix/bkpr.nix {
            inherit libghostty-vt src version;
            toolchain = toolchainFor pkgs;
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
        in
        {
          default = pkgs.mkShell {
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
        }
      );

      formatter = forAllSystems (system: (pkgsFor system).nixfmt);
    };
}
