{
  description = "doiget — open-access academic paper fetcher and stdio MCP server";

  inputs = {
    nixpkgs.url     = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
    rust-overlay = {
      url   = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = { self, nixpkgs, flake-utils, rust-overlay }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs     = import nixpkgs { inherit system overlays; };

        # Latest stable, as rust-toolchain.toml does. The flake used to pin
        # 1.86, below the 1.88 that rmcp 3.x requires, so it could not build
        # the tree (#501); the declared MSRV is now 1.88.
        rustToolchain = pkgs.rust-bin.stable.latest.default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        # One version, read from the workspace: a hand-kept copy here had
        # drifted to 0.7.2-beta.1.
        cargoToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
          perl          # ring crate's build script needs perl
        ];

        # Darwin needs no explicit framework inputs: current nixpkgs puts the
        # SDK in the default stdenv, and `darwin.apple_sdk.frameworks` is gone.
        buildInputs = [ ];

        doiget = pkgs.rustPlatform.buildRustPackage {
          pname   = "doiget";
          version = cargoToml.workspace.package.version;

          src = pkgs.lib.cleanSource ./.;

          cargoLock.lockFile = ./Cargo.lock;

          # Build only the CLI binary with the public Tier-1 OA feature set.
          cargoBuildFlags = [
            "-p" "doiget-cli"
            "--no-default-features"
            "--features" "oa-only"
          ];

          inherit nativeBuildInputs buildInputs;

          # Tests that hit the network are skipped in the Nix sandbox.
          doCheck = false;

          meta = with pkgs.lib; {
            description = "Open-access academic paper fetcher and stdio MCP server";
            homepage    = "https://github.com/QAtlasHub/doiget";
            license     = licenses.mit;
            maintainers = [];
            platforms   = platforms.unix ++ platforms.windows;
            mainProgram = "doiget";
          };
        };
      in
      {
        # `nix build` / `nix profile install`
        packages.default = doiget;
        packages.doiget  = doiget;

        # `nix run . -- fetch <doi>`
        apps.default = flake-utils.lib.mkApp { drv = doiget; };
        apps.doiget  = flake-utils.lib.mkApp { drv = doiget; };

        # `nix develop`
        devShells.default = pkgs.mkShell {
          inherit buildInputs;
          nativeBuildInputs = nativeBuildInputs ++ (with pkgs; [
            cargo-deny
            cargo-nextest
            cargo-llvm-cov
            taplo         # TOML formatter / linter used in CI
          ]);
          RUST_BACKTRACE = "1";
        };
      }
    );
}
