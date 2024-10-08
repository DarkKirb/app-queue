{
  description = "rust-template";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";

    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
    };

    cargo2nix = {
      url = "github:cargo2nix/cargo2nix";
      inputs.nixpkgs.follows = "nixpkgs";
      inputs.flake-utils.follows = "flake-utils";
      inputs.rust-overlay.follows = "rust-overlay";
    };
  };

  outputs = {
    self,
    nixpkgs,
    flake-utils,
    rust-overlay,
    cargo2nix,
    ...
  } @ inputs:
    flake-utils.lib.eachSystem ["x86_64-linux"] (system: let
      overlays = [
        cargo2nix.overlays.default
        (import rust-overlay)
      ];
      pkgs = import nixpkgs {
        inherit system overlays;
      };
      rustPkgs = pkgs.rustBuilder.makePackageSet {
        packageFun = import ./Cargo.nix;
        rustChannel = "1.78.0";
        packageOverrides = pkgs: pkgs.rustBuilder.overrides.all;
      };
    in rec {
      devShells.default = with pkgs;
        mkShell {
          buildInputs = [
            (rust-bin.stable.latest.default.override {
              extensions = ["rust-src"];
            })
            cargo2nix.packages.${system}.cargo2nix
            sqlx-cli
            alejandra
            cargo-release
            sqlite
          ];
        };
      packages = {
        app-queue = rustPkgs.workspace.app-queue {};
      };
      nixosModules.default = import ./nixos {
        inherit inputs system;
      };
      hydraJobs = packages;
      formatter = pkgs.alejandra;
    });
}
