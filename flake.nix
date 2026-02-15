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
      crossSystem = nixpkgs.lib.systems.examples.musl64 // { useLLVM = false; };

      pkgs = import nixpkgs {
        inherit localSystem crossSystem;
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
                final.fenix.targets.${crossSystem.config}.stable.rust-std
              ];

            rustStdenv = final.pkgsBuildHost.llvmPackages_18.stdenv;
            rustLinker = final.pkgsBuildHost.llvmPackages_18.lld;

            naerskBuild = (naersk.lib.${localSystem}.override {
              cargo = final.rustToolchain;
              rustc = final.rustToolchain;
              stdenv = final.rustStdenv;
            }).buildPackage;
          })
        ];
      };

      inherit (pkgs.lib) mkForce;
    in
    rec {
      packages.claudevil = pkgs.naerskBuild {
        pname = "claudevil";
        src = pkgs.gitignoreSource ./.;

        nativeBuildInputs = with pkgs; [
          pkgsBuildBuild.protobuf
          pkgsBuildBuild.pkg-config
          rustStdenv.cc
          rustLinker
        ];

        hardeningDisable = [ "fortify" ];

        CARGO_BUILD_TARGET = crossSystem.config;
        RUSTFLAGS = "-C linker-flavor=ld.lld -C target-feature=+crt-static";
      };

      packages.default = packages.claudevil;

      packages.site = pkgs.pkgsBuildBuild.stdenv.mkDerivation {
        pname = "claudevil-site";
        version = "0.1.0";
        src = ./site;
        nativeBuildInputs = [ pkgs.pkgsBuildBuild.zola ];
        buildPhase = "zola build";
        installPhase = "cp -r public $out";
      };

      packages.container = pkgs.pkgsBuildBuild.dockerTools.buildLayeredImage {
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
        program = "${pkgs.pkgsBuildBuild.writeShellApplication {
          name = "claudevil-site-serve";
          runtimeInputs = [pkgs.pkgsBuildBuild.zola];
          text = ''
            exec zola --root site serve "$@"
          '';
        }}/bin/claudevil-site-serve";
      };

      apps.trufflehog = {
        type = "app";
        program = "${pkgs.pkgsBuildBuild.writeShellApplication {
          name = "claudevil-trufflehog";
          runtimeInputs = [pkgs.pkgsBuildBuild.trufflehog];
          text = ''
            exec trufflehog git "file://$(git rev-parse --show-toplevel)" --since-commit HEAD --fail "$@"
          '';
        }}/bin/claudevil-trufflehog";
      };

      apps.audit = {
        type = "app";
        program = "${pkgs.pkgsBuildBuild.writeShellApplication {
          name = "claudevil-audit";
          runtimeInputs = [pkgs.pkgsBuildBuild.cargo-audit];
          text = ''
            exec cargo-audit audit "$@"
          '';
        }}/bin/claudevil-audit";
      };

      checks = {
        inherit (packages) claudevil;

        pre-commit-check = pre-commit-hooks.lib.${localSystem}.run {
          src = ./.;
          hooks = {
            statix.enable = true;
            deadnix.enable = true;
            nixpkgs-fmt.enable = true;
            shellcheck.enable = true;

            shfmt = {
              enable = true;
              entry = mkForce "${pkgs.pkgsBuildBuild.shfmt}/bin/shfmt -i 2 -sr -d -s -l";
              files = "\\.sh$";
            };

            rustfmt = {
              enable = true;
              entry = mkForce "${pkgs.pkgsBuildBuild.rustToolchain}/bin/cargo fmt -- --check --color=always";
            };

            clippy = {
              enable = true;
              entry = mkForce "${pkgs.pkgsBuildBuild.rustToolchain}/bin/cargo clippy -- -D warnings";
            };

            cargo-check = {
              enable = true;
              entry = mkForce "${pkgs.pkgsBuildBuild.rustToolchain}/bin/cargo check";
            };

            taplo = {
              enable = true;
              entry = mkForce "${pkgs.pkgsBuildBuild.taplo}/bin/taplo fmt";
              types = [ "toml" ];
            };
          };
        };
      };

      devShells.default = pkgs.mkShell {
        nativeBuildInputs = with pkgs.pkgsBuildBuild; [
          rustToolchain
          cargo-audit
          cargo-llvm-cov
          stdenv.cc
          llvmPackages_18.lld
          pkg-config
          protobuf
          cacert
          deadnix
          git
          nixpkgs-fmt
          statix
          taplo
          trufflehog
          zola
        ];

        inherit (self.checks.${localSystem}.pre-commit-check) shellHook;
      };
    });
}
