{
  description = "Hemera's Home Automation System";
  inputs = {
    nixpkgs.url = "nixpkgs/nixos-22.05";
    flake-utils = {
      url = "github:numtide/flake-utils";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    crane = {
      url = "github:ipetkov/crane";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs = {
        nixpkgs.follows = "nixpkgs";
        flake-utils.follows = "flake-utils";
      };
    };
  };

  outputs = { self, nixpkgs, crane, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };

        rustTarget = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;
        craneLib = (crane.mkLib pkgs).overrideToolchain rustTarget;

        tomlInfo = craneLib.crateNameFromCargoToml { cargoToml = ./Cargo.toml; };
        inherit (tomlInfo) pname version;
        src = craneLib.cleanCargoSource ./.;

        cargoArtifacts = craneLib.buildDepsOnly {
          inherit src;
        };

        hhas = craneLib.buildPackage {
          inherit cargoArtifacts src version;
        };

      in
      rec {
        checks = {
          inherit hhas;

          hhas-clippy = craneLib.cargoClippy {
            inherit cargoArtifacts src;
            cargoClippyExtraArgs = "-- --deny warnings";
          };

          hhas-fmt = craneLib.cargoFmt {
            inherit src;
          };
        };

        packages.hhas = hhas;
        packages.default = packages.hhas;

        apps.hhas = flake-utils.lib.mkApp {
          name = "hhas";
          drv = hhas;
        };
        apps.default = apps.hhas;

        devShells.default = devShells.hhas;
        devShells.hhas = pkgs.mkShell {
          buildInputs = [
            pkgs.pkg-config
            pkgs.openssl
          ];

          nativeBuildInputs = [
            rustTarget

            pkgs.yt-dlp
            pkgs.mpv

            pkgs.cargo-msrv
            pkgs.cargo-deny
            pkgs.cargo-expand
            pkgs.cargo-bloat
            pkgs.cargo-fuzz
          ];
        };
      }
    );
}
