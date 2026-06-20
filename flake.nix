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

        # Pin to the workspace MSRV declared in Cargo.toml.
        rustToolchain = pkgs.rust-bin.stable."1.86.0".default.override {
          extensions = [ "rust-src" "rust-analyzer" ];
        };

        nativeBuildInputs = with pkgs; [
          rustToolchain
          pkg-config
          perl          # ring crate's build script needs perl
        ];

        buildInputs = with pkgs; lib.optionals stdenv.isDarwin [
          darwin.apple_sdk.frameworks.Security
          darwin.apple_sdk.frameworks.SystemConfiguration
        ];

        doiget = pkgs.rustPlatform.buildRustPackage {
          pname   = "doiget";
          # Keep in sync with [workspace.package] version in Cargo.toml.
          version = "0.7.2-beta.1";

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
            homepage    = "https://github.com/sotashimozono/doiget";
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
