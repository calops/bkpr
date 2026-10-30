{
  description = "bkpr — pane-first native terminal multiplexer development";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/b1b875982b17dabde9b4a37f3e229e74913e6db3";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, rust-overlay, ... }:
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
    in
    {
      # The pinned engine preparation currently runs outside the Nix sandbox.
      # Package/app outputs return only after its full dependency closure is
      # available to reproducible, offline native builds.
      devShells = forAllSystems (
        system:
        let
          pkgs = pkgsFor system;
        in
        {
          default = pkgs.mkShell {
            packages = [
              (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml)
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
