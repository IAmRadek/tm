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

      # The timebuzzer knob's window; Darwin only, because src/macos.rs calls AppKit.
      # It runs tm for every write, at the store path baked in through TM_BIN, so it needs no PATH.
      mkBuzzer = pkgs: tm:
        pkgs.rustPlatform.buildRustPackage {
          pname = "tm-buzzer";
          inherit version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = [ "--package" "tm-buzzer" ];
          cargoTestFlags = [ "--package" "tm-buzzer" ];
          # raylib-sys builds raylib with cmake and generates bindings with bindgen
          nativeBuildInputs = [ pkgs.cmake pkgs.rustPlatform.bindgenHook ];
          # cmake is for raylib-sys's build script, not for this crate
          dontUseCmakeConfigure = true;
          env.TM_BIN = "${tm}/bin/tm";
          meta.mainProgram = "tm-buzzer";
        };
    in {
      packages = forAllSystems (pkgs:
        let tm = mkPackage pkgs "tm";
        in {
          inherit tm;
          default = tm;
        } // lib.optionalAttrs pkgs.stdenv.hostPlatform.isDarwin {
          tm-daemon = mkPackage pkgs "tm-daemon";
          tm-buzzer = mkBuzzer pkgs tm;
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
        tm-buzzer = mkBuzzer final final.tm;
      };

      darwinModules.default = { config, lib, pkgs, ... }:
      let
        cfg = config.services.tm-daemon;
        buzzer = config.services.tm-buzzer;
      in {
        options.services.tm-daemon = {
          enable = lib.mkEnableOption "tm time tracker menu bar daemon";
          package = lib.mkOption {
            type = lib.types.package;
            default = self.packages.${pkgs.stdenv.hostPlatform.system}.tm-daemon;
            defaultText = lib.literalExpression "tm.packages.\${system}.tm-daemon";
          };
        };

        options.services.tm-buzzer = {
          enable = lib.mkEnableOption "tm-buzzer, the timebuzzer knob's window for tm";
          package = lib.mkOption {
            type = lib.types.package;
            default = self.packages.${pkgs.stdenv.hostPlatform.system}.tm-buzzer;
            defaultText = lib.literalExpression "tm.packages.\${system}.tm-buzzer";
          };
        };

        config = lib.mkMerge [
          (lib.mkIf cfg.enable {
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
          })
          (lib.mkIf buzzer.enable {
            launchd.user.agents.tm-buzzer = {
              serviceConfig = {
                Label = "com.iamradek.tm-buzzer";
                ProgramArguments = [ "${buzzer.package}/bin/tm-buzzer" ];
                RunAtLoad = true;
                # the app never exits on its own, so an exit is a crash or a kill; it restarts
                KeepAlive = true;
                # a window that answers the knob at once, so launchd does not throttle it
                ProcessType = "Interactive";
                StandardOutPath = "/tmp/tm-buzzer.log";
                StandardErrorPath = "/tmp/tm-buzzer.err";
              };
            };
          })
        ];
      };
    };
}
