{
  description = "A Discord bot made for my discord server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    rust-overlay.url = "github:oxalica/rust-overlay";
    flake-utils.url = "github:numtide/flake-utils";
    crane.url = "github:ipetkov/crane";
  };

  outputs =
    {
      self,
      nixpkgs,
      rust-overlay,
      flake-utils,
      crane,
      ...
    }:
    flake-utils.lib.eachDefaultSystem (
      system:
      let
        overlays = [ (import rust-overlay) ];
        pkgs = import nixpkgs {
          inherit system overlays;
        };
        inherit (pkgs) lib;

        # Release toolchain comes straight from rust-toolchain.toml (stable),
        # using the *default* profile only. We deliberately do NOT pull in
        # rust-src / rustc-dev / llvm-tools-preview here: the rust-src extension
        # makes rustc bake toolchain store paths into panic-location strings,
        # which Nix's reference scanner then treats as runtime references and
        # drags the entire ~2.6 GiB toolchain into the closure of a ~15 MiB
        # binary. The dev shell below keeps the full kit; it just never ships.
        rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml;

        craneLib = (crane.mkLib pkgs).overrideToolchain rustToolchain;

        # Keep Cargo sources plus the runtime asset dirs the build copies into
        # $out. craneLib.filterCargoSources alone would strip web/ & migrations/.
        src = lib.cleanSourceWith {
          src = ./.;
          filter =
            path: type:
            (craneLib.filterCargoSources path type)
            || (builtins.match ".*/(web|migrations)(/.*)?" path != null);
        };

        commonArgs = {
          inherit src;
          strictDeps = true;

          nativeBuildInputs = with pkgs; [
            pkg-config
          ];

          buildInputs = with pkgs; [
            openssl
            ffmpeg
            libqalculate
          ];
        };

        # Compile (and cache) the dependency graph once; crane reuses this
        # across rebuilds of the crate itself.
        cargoArtifacts = craneLib.buildDepsOnly commonArgs;

        tricked-bot = craneLib.buildPackage (
          commonArgs
          // {
            inherit cargoArtifacts;

            nativeBuildInputs =
              commonArgs.nativeBuildInputs
              ++ (
                with pkgs;
                [
                  makeWrapper
                  removeReferencesTo
                ]
              );

            postInstall = ''
              mkdir -p $out/share/tricked-bot
              cp -r web $out/share/tricked-bot/
              wrapProgram $out/bin/tricked-bot \
                --prefix PATH : ${lib.makeBinPath [ pkgs.ffmpeg pkgs.libqalculate ]}
            '';

            # Belt-and-suspenders: scrub any residual toolchain references the
            # compiler embedded so the runtime closure stays minimal.
            postFixup = ''
              find $out -type f -executable -exec \
                remove-references-to -t ${rustToolchain} {} +
            '';

            meta = with lib; {
              description = "A simple discord bot made for my discord";
              homepage = "https://discord.gg/mY8zTARu4g";
              license = licenses.asl20;
              maintainers = [ ];
              mainProgram = "tricked-bot";
            };
          }
        );

        # Dev shell keeps the full toolchain (rust-src for rust-analyzer, etc.)
        # plus nightly tooling. None of this ends up in the release closure.
        rustDevToolchain = pkgs.rust-bin.beta.latest.default.override {
          extensions = [
            "rust-src"
            "rust-analyzer"
            "clippy"
            "rustc-dev"
            "llvm-tools-preview"
          ];
        };
        rustNightlyToolchain = pkgs.rust-bin.nightly.latest.default;
      in
      {
        packages = {
          default = tricked-bot;
          inherit tricked-bot;
        };

        overlays.default = final: prev: {
          inherit tricked-bot;
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
              rustDevToolchain
              rustNightlyToolchain
              cargo-udeps
              ffmpeg
              libqalculate
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
