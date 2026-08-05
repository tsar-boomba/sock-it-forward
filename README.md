# `sock-it-forward`

Dumb public -> private TCP socket forwarding using [iroh](https://www.iroh.computer/) to securely move bytes between the two hosts.

## Nix Flake

I use nix on my server, so I made a flake to easily build and run this as a systemd service. Here's little example config:

```nix
{ config, pkgs, ... }:
{
  # ... your existing config ...

  services.sock-it-forward = {
    enable = true;
    mode = "private";
    private = {
      publicSideKey = "PASTE_PUBLIC_SIDE_PUBKEY_HERE";
      mappings = [
        "8080:localhost:80"
        "2222:localhost:22"
      ];
    };
  };
}
```
