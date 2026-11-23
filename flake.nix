# SPDX-License-Identifier: AGPL-3.0-or-later
{
  description = "unemu — multi-platform front-end devShell (Rust + Backlog.md + tooling)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    # Local-first task tracker. Provides the `backlog` CLI.
    # Do NOT add `.inputs.nixpkgs.follows` here — the bun2nix build wants its pinned nixpkgs.
    backlog-md.url = "github:MrLesk/Backlog.md";
    # The Rust toolchain: the latest stable release as upstream binaries, so
    # moving to a new Rust never rebuilds the rest of nixpkgs.
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    {
      nixpkgs,
      flake-utils,
      backlog-md,
      rust-overlay,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [ (import rust-overlay) ];
        };
        # Keep [workspace.package] rust-version in Cargo.toml equal to this.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustfmt"
          ];
        };
      in
      {
        devShells.default = pkgs.mkShell {
          packages = [
            backlog-md.packages.${system}.default # `backlog` — local-first task tracker
            pkgs.pre-commit
            pkgs.git
            pkgs.gnupg # uid-guard.sh verifies the signing key UID

            # Rust and dependency checks.
            rustToolchain
            pkgs.cargo-deny
            # Runs every test in its own process and test binaries side by
            # side; scripts/affected-tests.sh uses it when present.
            pkgs.cargo-nextest

            # The media boot-sector source test assembles its checked-in bytes.
            pkgs.nasm

            # Recipe media uses mformat, mcopy and mdir.
            pkgs.mtools
            # FreeDOS source archives need deterministic GNU tar and ISO extraction.
            pkgs.gnutar
            pkgs.python3
            pkgs.coreutils # sha256sum for fixture verification and export-layout.sh

            # --- fixture downloads and archive extraction (vendor/*/fetch.sh) ---
            # Pinned instead of taken from the host, so every fixture script
            # runs the same tools on macOS and Linux. Only curl's `bin` output:
            # mkShell would otherwise add its `dev` output, whose headers and
            # pkg-config files reach the C compiles inside Rust builds.
            pkgs.curl.bin
            pkgs.unzip # Info-ZIP UnZip 6.00

            pkgs.jq

            # scripts/export-layout.sh decodes +zstd layers without unemu.
            pkgs.zstd
          ] ++ pkgs.lib.optionals pkgs.stdenv.isLinux [
            # cpal's Linux backend discovers ALSA through pkg-config.
            pkgs.pkg-config
            pkgs.alsa-lib
            pkgs.glibc.bin # locale for deterministic media exports
            pkgs.xdg-utils # reveal the diagnostics folder on Linux
          ];
          # Downloads trust the pinned Mozilla roots, not the host's bundle: an
          # old container's bundle lacks roots that current mirrors chain to.
          # Nixpkgs' OpenSSL (curl, git) reads NIX_SSL_CERT_FILE before
          # SSL_CERT_FILE; the curl program itself prefers CURL_CA_BUNDLE to a
          # host's SSL_CERT_FILE. cacert stays out of `packages`, whose setup
          # hook would export SSL_CERT_FILE: the registry client's native-roots
          # loader would then read that file instead of the macOS keychain.
          NIX_SSL_CERT_FILE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
          CURL_CA_BUNDLE = "${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt";
        };
      }
    );
}
