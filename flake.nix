{
  description = "A devShell example";

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
      in
      {
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
              rust-bin.beta.latest.default
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
