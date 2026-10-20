{
  description = "tm - minimal CLI time tracker";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";

  outputs = { self, nixpkgs }:
    let
      lib = nixpkgs.lib;
      systems = [ "aarch64-darwin" "aarch64-linux" "x86_64-linux" ];
      forAllSystems = f: lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
      version = (lib.importTOML ./Cargo.toml).package.version;

      # Builds and tests one crate of the Cargo workspace.
      mkPackage = pkgs: pname:
        pkgs.rustPlatform.buildRustPackage {
          inherit pname version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--package" pname ];
          cargoTestFlags = [ "--package" pname ];
          nativeBuildInputs = [ pkgs.installShellFiles ];
          postInstall = lib.optionalString (pname == "tm") ''
            installShellCompletion --cmd tm \
              --bash <($out/bin/tm completions bash) \
              --fish <($out/bin/tm completions fish) \
              --zsh <($out/bin/tm completions zsh)
          '';
        };
    in {
      packages = forAllSystems (pkgs:
        let tm = mkPackage pkgs "tm";
        in {
          inherit tm;
          default = tm;
        } // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          tm-daemon = mkPackage pkgs "tm-daemon";
        });

      checks = forAllSystems (pkgs: self.packages.${pkgs.stdenv.hostPlatform.system});

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          inputsFrom = [ self.packages.${pkgs.stdenv.hostPlatform.system}.tm ];
          packages = [ pkgs.clippy pkgs.rustfmt pkgs.rust-analyzer ];
        };
      });

      overlays.default = final: prev: {
        tm = mkPackage final "tm";
        tm-daemon = mkPackage final "tm-daemon";
      };

      darwinModules.default = { config, lib, pkgs, ... }:
      let
        cfg = config.services.tm-daemon;
      in {
        options.services.tm-daemon = {
          enable = lib.mkEnableOption "tm time tracker menu bar daemon";
          package = lib.mkOption {
            type = lib.types.package;
            default = self.packages.${pkgs.stdenv.hostPlatform.system}.tm-daemon;
            defaultText = lib.literalExpression "tm.packages.\${system}.tm-daemon";
          };
        };

        config = lib.mkIf cfg.enable {
          launchd.user.agents.tm-daemon = {
            serviceConfig = {
              Label = "com.iamradek.tm-daemon";
              ProgramArguments = [ "${cfg.package}/bin/tm-daemon" ];
              RunAtLoad = true;
              KeepAlive = true;
              StandardOutPath = "/tmp/tm-daemon.log";
              StandardErrorPath = "/tmp/tm-daemon.err";
            };
          };
        };
      };
    };
}
