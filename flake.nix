{
  description = "A Discord bot made for my discord server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        rustToolchain = pkgs.rust-bin.beta.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustc-dev"
            "llvm-tools-preview"
          ];
        };
      in
      {
        packages = {
          default = self.packages.${system}.tricked-bot;

          tricked-bot = pkgs.rustPlatform.buildRustPackage {
            pname = "tricked-bot";
            version = "1.4.0";

            src = ./.;

            cargoLock = {
              lockFile = ./Cargo.lock;
            };

            nativeBuildInputs = with pkgs; [
              pkg-config
              rustToolchain
            ];

            buildInputs = with pkgs; [
              openssl
            ];

            meta = with pkgs.lib; {
              description = "A simple discord bot made for my discord";
              homepage = "https://discord.gg/mY8zTARu4g";
              license = licenses.asl20;
              maintainers = [ ];
            };
          };
        };

        overlays.default = final: prev: {
          tricked-bot = self.packages.${system}.tricked-bot;
        };

        devShells.default =
          with pkgs;
          mkShell {
            buildInputs = [
              openssl
              pkg-config
              eza
              fd
              clang
              mold
              rustToolchain
            ];

            LD_LIBRARY_PATH = lib.makeLibraryPath [
              openssl
            ];

            shellHook = ''
              if [ -f .env ]; then
                set -a
                source .env
                set +a
                echo "Loaded environment variables from .env"
              fi
            '';
          };
      }
    );
}
