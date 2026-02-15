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
      # Derive the musl cross-target from the build architecture so the
      # same flake works on both x86_64 and aarch64 hosts.
      arch = builtins.head (builtins.split "-" localSystem);
      crossSystem = {
        config = "${arch}-unknown-linux-musl";
        useLLVM = false;
      };

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

      # Shared naersk args for the musl cross-build. Used by both the
      # package (no tests) and the check (with tests).
      naerskArgs = {
        pname = "claudevil";
        src = pkgs.gitignoreSource ./.;

        nativeBuildInputs = with pkgs; [
          pkgsBuildBuild.pkg-config
          rustStdenv.cc
          rustLinker
        ];

        hardeningDisable = [ "fortify" ];

        CARGO_BUILD_TARGET = crossSystem.config;
        # Point the linker at the cross-GCC's static libstdc++ so usearch's
        # C++ runtime symbols (operator new, __cxa_guard_*, exceptions) resolve.
        RUSTFLAGS = "-C linker-flavor=ld.lld -C target-feature=+crt-static -L ${pkgs.stdenv.cc.cc}/${crossSystem.config}/lib";
      };
    in
    rec {
      packages.claudevil = pkgs.naerskBuild naerskArgs;

      packages.default = packages.claudevil;

      packages.site = pkgs.pkgsBuildBuild.stdenv.mkDerivation {
        name = "claudevil-site";
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
        # Build + test with the musl cross-toolchain, then verify the
        # resulting binary is statically linked.
        claudevil =
          let
            built = pkgs.naerskBuild (naerskArgs // { doCheck = true; });
          in
          pkgs.pkgsBuildBuild.runCommand "claudevil-static-check" { } ''
            ${pkgs.pkgsBuildBuild.file}/bin/file ${built}/bin/claudevil \
              | tee /dev/stderr \
              | grep -q "statically linked"
            touch $out
          '';

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
        nativeBuildInputs = (with pkgs.pkgsBuildBuild; [
          rustToolchain
          actionlint
          cargo-audit
          cargo-llvm-cov
          pkg-config
          cacert
          deadnix
          git
          nixpkgs-fmt
          statix
          taplo
          trufflehog
          zola
        ]) ++ [
          # Musl-targeting clang and lld — same toolchain as the nix build.
          pkgs.rustStdenv.cc
          pkgs.rustLinker
        ];

        hardeningDisable = [ "fortify" ];

        # Always target musl so cargo build/test/clippy produce static binaries.
        CARGO_BUILD_TARGET = crossSystem.config;
        RUSTFLAGS = "-C linker-flavor=ld.lld -C target-feature=+crt-static -L ${pkgs.stdenv.cc.cc}/${crossSystem.config}/lib";

        inherit (self.checks.${localSystem}.pre-commit-check) shellHook;
      };
    });
}
