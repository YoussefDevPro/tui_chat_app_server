{
  description = "a tui chat app, now u can talk with ur friends witout having to install a desktop environement >:3";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs";
    flake-utils.url = "github:numtide/flake-utils";
    cargo2nix.url = "github:cargo2nix/cargo2nix";
  };

  outputs = { self, nixpkgs, flake-utils, cargo2nix, ... }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        rustPkgs = pkgs.callPackage ./Cargo.nix { };
        rustWorkspace = rustPkgs.workspace ./.;
      in {
        packages.default = rustWorkspace.rootCrate.build;
        devShells.default = rustWorkspace.rootCrate.shell;
      }
    );
}

