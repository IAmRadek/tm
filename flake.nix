{
  description = "tm - minimal CLI time tracker";

  outputs = { self }: {
    overlays.default = final: prev: {
      tm = final.rustPlatform.buildRustPackage {
        pname = "tm";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "--package" "tm" ];
        doCheck = false;
      };

      tm-daemon = final.rustPlatform.buildRustPackage {
        pname = "tm-daemon";
        version = "0.1.0";
        src = self;
        cargoLock.lockFile = ./Cargo.lock;
        cargoBuildFlags = [ "--package" "tm-daemon" ];
        buildInputs = with final.darwin.apple_sdk.frameworks; [
          AppKit
          Foundation
        ];
        doCheck = false;
      };
    };

    darwinModules.default = { config, lib, pkgs, ... }:
    let
      cfg = config.services.tm-daemon;
    in {
      options.services.tm-daemon = {
        enable = lib.mkEnableOption "tm time tracker menu bar daemon";
        package = lib.mkOption {
          type = lib.types.package;
          default = pkgs.tm-daemon;
          defaultText = lib.literalExpression "pkgs.tm-daemon";
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
