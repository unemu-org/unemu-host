# SPDX-License-Identifier: AGPL-3.0-or-later
{
  description = "unemu-host — host session libraries devShell";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Keep the imported input graph and flake.lock unchanged. This input is
    # unused by the shell; work is tracked outside this repository.
    backlog-md.url = "github:MrLesk/Backlog.md";
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { nixpkgs, flake-utils, rust-overlay, ... }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        # flake.lock pins the same Rust release as unemu (1.99.1).
        # Keep Cargo.toml's rust-version compatible with this toolchain.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" "clippy" "rustfmt" ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            rustToolchain
            pkgs.cargo-deny
            pkgs.git
            pkgs.jq # layer-guard.sh reads Cargo metadata
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            pkgs.pkg-config
            pkgs.alsa-lib
          ];
          NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        };
      }
    );
}
