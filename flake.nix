{
  description = "claudevil - single-binary MCP server for RAG over local files";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

    fenix = {
      url = "github:nix-community/fenix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    naersk = {
      url = "github:nmattia/naersk";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    flake-utils.url = "github:numtide/flake-utils";

    gitignore = {
      url = "github:hercules-ci/gitignore.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };

    pre-commit-hooks = {
      url = "github:cachix/pre-commit-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    { self
    , nixpkgs
    , fenix
    , naersk
    , flake-utils
    , gitignore
    , pre-commit-hooks
    , ...
    }:
    flake-utils.lib.eachDefaultSystem (localSystem:
    let
      pkgs = import nixpkgs {
        inherit localSystem;
        overlays = [
          fenix.overlays.default
          gitignore.overlay
          (final: _: {
            rustToolchain =
              let
                fenixPackages = fenix.packages.${localSystem};
              in
              final.fenix.combine [
                fenixPackages.stable.clippy
                fenixPackages.stable.llvm-tools-preview
                fenixPackages.stable.rust-src
                fenixPackages.stable.rust-analyzer
                fenixPackages.stable.rustfmt
                fenixPackages.minimal.cargo
                fenixPackages.minimal.rustc
              ];

            naerskBuild = (naersk.lib.${localSystem}.override {
              cargo = final.rustToolchain;
              rustc = final.rustToolchain;
            }).buildPackage;
          })
        ];
      };

      inherit (pkgs.lib) mkForce;

      fenixPackages = fenix.packages.${localSystem};

      # Nightly toolchain for cargo-udeps (which requires unstable features).
      # Wrapped so nightly doesn't leak into the normal dev PATH.
      nightlyToolchain = fenixPackages.latest.toolchain;
      cargoUdeps = pkgs.writeShellApplication {
        name = "cargo-udeps";
        text = ''
          PATH="${nightlyToolchain}/bin:$PATH" exec ${pkgs.cargo-udeps}/bin/cargo-udeps "$@"
        '';
      };

      naerskArgs = {
        pname = "claudevil";
        src = pkgs.gitignoreSource ./.;

        nativeBuildInputs = with pkgs; [
          pkg-config
          stdenv.cc
        ];
      };
    in
    rec {
      packages.claudevil = pkgs.naerskBuild naerskArgs;

      packages.default = packages.claudevil;

      packages.site = pkgs.stdenv.mkDerivation {
        name = "claudevil-site";
        src = ./site;
        nativeBuildInputs = [ pkgs.zola ];
        buildPhase = "zola build";
        installPhase = "cp -r public $out";
      };

      packages.container = pkgs.dockerTools.buildLayeredImage {
        name = "claudevil";
        tag = "latest";
        contents = [ packages.claudevil ];
        config = {
          Entrypoint = [ "${packages.claudevil}/bin/claudevil" ];
        };
      };

      apps.default = flake-utils.lib.mkApp { drv = packages.claudevil; };

      apps.site = {
        type = "app";
        program = "${pkgs.writeShellApplication {
          name = "claudevil-site-serve";
          runtimeInputs = [ pkgs.zola ];
          text = ''
            exec zola --root site serve "$@"
          '';
        }}/bin/claudevil-site-serve";
      };

      apps.trufflehog = {
        type = "app";
        program = "${pkgs.writeShellApplication {
          name = "claudevil-trufflehog";
          runtimeInputs = [ pkgs.trufflehog ];
          text = ''
            exec trufflehog git "file://$(git rev-parse --show-toplevel)" --since-commit HEAD --fail "$@"
          '';
        }}/bin/claudevil-trufflehog";
      };

      apps.audit = {
        type = "app";
        program = "${pkgs.writeShellApplication {
          name = "claudevil-audit";
          runtimeInputs = [ pkgs.cargo-audit ];
          text = ''
            exec cargo-audit audit "$@"
          '';
        }}/bin/claudevil-audit";
      };

      checks = {
        claudevil = pkgs.naerskBuild (naerskArgs // { doCheck = true; });

        pre-commit-check = pre-commit-hooks.lib.${localSystem}.run {
          src = ./.;
          hooks = {
            actionlint.enable = true;
            statix.enable = true;
            deadnix.enable = true;
            nixpkgs-fmt.enable = true;
            shellcheck.enable = true;

            shfmt = {
              enable = true;
              entry = mkForce "${pkgs.shfmt}/bin/shfmt -i 2 -sr -d -s -l";
              files = "\\.sh$";
            };

            rustfmt = {
              enable = true;
              entry = mkForce "${pkgs.rustToolchain}/bin/cargo fmt -- --check --color=always";
            };

            clippy = {
              enable = true;
              entry = mkForce "${pkgs.rustToolchain}/bin/cargo clippy -- -D warnings";
            };

            cargo-check = {
              enable = true;
              entry = mkForce "${pkgs.rustToolchain}/bin/cargo check";
            };

            cargo-udeps = {
              enable = true;
              entry = mkForce "${cargoUdeps}/bin/cargo-udeps udeps --all-targets";
              pass_filenames = false;
              types = [ "rust" ];
            };

            taplo = {
              enable = true;
              entry = mkForce "${pkgs.taplo}/bin/taplo fmt";
              types = [ "toml" ];
            };
          };
        };
      };

      devShells.default = pkgs.mkShell {
        nativeBuildInputs = [
          pkgs.rustToolchain
          cargoUdeps
          pkgs.actionlint
          pkgs.cargo-audit
          pkgs.cargo-llvm-cov
          pkgs.pkg-config
          pkgs.cacert
          pkgs.deadnix
          pkgs.git
          pkgs.nixpkgs-fmt
          pkgs.statix
          pkgs.taplo
          pkgs.trufflehog
          pkgs.zola
          pkgs.nodejs
        ];

        # usearch links against C++ — ensure libstdc++ is available at runtime
        LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath [ pkgs.stdenv.cc.cc.lib ];

        inherit (self.checks.${localSystem}.pre-commit-check) shellHook;
      };
    });
}
