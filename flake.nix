{
  inputs.nixpkgs.url = "github:nixos/nixpkgs/nixos-unstable";

  outputs = { self, nixpkgs }:
    let
      forAllSystems = nixpkgs.lib.genAttrs [
        "x86_64-linux" "aarch64-linux" "aarch64-darwin" "x86_64-darwin"
      ];
    in {
      packages = forAllSystems (system:
        let pkgs = nixpkgs.legacyPackages.${system}; in {
          default = pkgs.rustPlatform.buildRustPackage {
            pname = "sock-it-forward";
            version = "0.1.0";
            src = self;
            cargoLock.lockFile = ./Cargo.lock;
          };
        });

      nixosModules.default = { config, lib, pkgs, ... }:
        let
          cfg = config.services.sock-it-forward;
          bin = "${self.packages.${pkgs.system}.default}/bin/sock-it-forward";
          stateDir = "/var/lib/sock-it-forward";

          args =
            if cfg.mode == "public" then
              [ "public" "--secret-key" "${stateDir}/secret.key" ]
              ++ lib.concatMap (a: [ "--addrs" a ]) cfg.public.addrs
              ++ lib.optionals (cfg.public.privateSideKey != null)
                  [ "--private-side-key" cfg.public.privateSideKey ]
            else
              [ "private" "--secret-key" "${stateDir}/secret.key"
                "--public-side-key" cfg.private.publicSideKey ]
              ++ lib.concatMap (m: [ "--map" m ]) cfg.private.mappings;
        in {
          options.services.sock-it-forward = {
            enable = lib.mkEnableOption "sock-it-forward tunnel";

            mode = lib.mkOption {
              type = lib.types.enum [ "public" "private" ];
              description = "Which side of the tunnel this machine runs.";
            };

            public = {
              addrs = lib.mkOption {
                type = lib.types.listOf lib.types.str;
                default = [ ];
                example = [ "0.0.0.0:4433" ];
                description = "Socket addresses to listen on (public mode).";
              };
              privateSideKey = lib.mkOption {
                type = lib.types.nullOr lib.types.str;
                default = null;
                description = "Only allowed private-side public key (Base64 or hex).";
              };
            };

            private = {
              publicSideKey = lib.mkOption {
                type = lib.types.str;
                description = "Public key of the public side (Base64 or hex).";
              };
              mappings = lib.mkOption {
                type = lib.types.listOf lib.types.str;
                default = [ ];
                example = [ "8080:127.0.0.1:80" ];
                description = "Port mappings (private mode).";
              };
            };
          };

          config = lib.mkIf cfg.enable {
            assertions = [
              {
                assertion = cfg.mode != "public" || cfg.public.addrs != [ ];
                message = "services.sock-it-forward: public mode requires at least one address in public.addrs";
              }
              {
                assertion = cfg.mode != "private" || cfg.private.mappings != [ ];
                message = "services.sock-it-forward: private mode requires at least one entry in private.mappings (--map is required)";
              }
            ];

            systemd.services.sock-it-forward = {
              description = "sock-it-forward (${cfg.mode} side)";
              wantedBy = [ "multi-user.target" ];
              after = [ "network-online.target" ];
              wants = [ "network-online.target" ];
              serviceConfig = {
                ExecStart = lib.escapeShellArgs ([ bin ] ++ args);
                StateDirectory = "sock-it-forward";
                Restart = "on-failure";
                RestartSec = 2;
                DynamicUser = true;
                ProtectSystem = "strict";
                ProtectHome = true;
                NoNewPrivileges = true;
              };
            };
          };
        };
    };
}