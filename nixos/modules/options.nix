# The handful of settings that differ between one build of this OS and
# another, so `nix eval .#nixosConfigurations.<name>.config.losos` says which
# values a build was given.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  inherit (lib) mkOption types;
  hostPlatform = pkgs.stdenv.hostPlatform;
in
{
  options.losos = {
    version = mkOption {
      type = types.strMatching "[0-9][0-9A-Za-z.~^-]*";
      example = "20260930.162538";
      description = ''
        The release version. It is baked into the image in three places that
        systemd-sysupdate reads back: the UKI's file name on the ESP, the
        partition label of the /usr and /usr-verity partitions, and
        IMAGE_VERSION in os-release. sysupdate orders versions with
        strverscmp(3), so a later build must compare greater.

        The flake sets it from the commit date, which is monotonic along a
        branch. It is not left at a constant default because the failure a
        constant produces is silent: every update would carry the same version
        as the running system, and sysupdate would never install one.
      '';
    };

    channel = mkOption {
      type = types.enum config.losos.channels;
      default = "nightly";
      description = ''
        The release cadence this build is for, and so the one a system
        installed from it follows until someone picks another in Settings,
        System (update-source.nix). `nightly` is every CI run on main; a
        weekly, monthly or yearly release is CI built again from the newest
        nightly's commit for that cadence (docs/releases.md). CI moves the
        channel's tag in GHCR to each release of it, and proxy/ serves that
        tag at `/updates/<channel>/`; the documentation site lists the
        channel's newest GitHub release at `updates/<channel>/<arch>/index`.
        flake.nix sets it from `LOSOS_CHANNEL`.
      '';
    };

    channels = mkOption {
      type = types.listOf (types.strMatching "[a-z][a-z0-9-]*");
      default = [
        "nightly"
        "weekly"
        "monthly"
        "yearly"
      ];
      description = ''
        Every release cadence: the prefix of its releases' tags, which is
        how website/release-index.js and .github/workflows/cadence.yml tell
        them apart, and what `losos-update-channel` accepts.
      '';
    };

    ping = {
      url = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "https://losos-desktop-proxy.dasmat.us/ping";
        description = ''
          Where the ping goes: the proxy's `/ping` (proxy/). The flake sets
          it from LOSOS_PROXY_URL, like `losos.update.baseUrl`; with none,
          nothing is sent and the choice screens stay hidden. A user turns
          it off for themselves with Settings, Privacy, which writes
          `privacy.usage_ping = false` (ping.nix).
        '';
      };
    };

    release.repository = mkOption {
      type = types.nullOr types.str;
      default = "losos-project/losos-desktop";
      example = "someone/losos-desktop";
      description = ''
        The GitHub repository, `owner/name`, whose release of this version
        the installer ISO installs (docs/releases.md). The flake sets it from
        GITHUB_REPOSITORY, so a fork's CI builds ISOs that read the fork's
        releases. `null` installs the channel's newest release instead.
      '';
    };

    report = {
      clientId = mkOption {
        type = types.nullOr types.str;
        default = null;
        example = "Iv23liAbCdEfGhIjKlMn";
        description = ''
          The client ID of the GitHub App the installer signs in with to
          report a failed install as an issue (docs/installer.md, "Reporting
          a failed install"). It is not a secret: GitHub shows it on the
          sign-in page. The flake sets it from LOSOS_REPORT_CLIENT_ID, a
          repository variable in CI; with none, the installer offers no
          report.
        '';
      };
      repository = mkOption {
        type = types.nullOr types.str;
        default = config.losos.release.repository;
        defaultText = lib.literalExpression "config.losos.release.repository";
        description = ''
          The repository, `owner/name`, that failed installs are reported to,
          the one the releases come from unless set.
        '';
      };
    };

    docs.url = mkOption {
      type = types.nullOr types.str;
      default = "https://losos-project.github.io/losos-desktop";
      description = ''
        The published documentation site, where derisk's error alerts open a
        section the image's own copy lacks (docs.nix). `null` keeps them to
        the copy on the machine.
      '';
    };

    update = {
      baseUrl = mkOption {
        type = types.str;
        default = "http://127.0.0.1:8485/";
        description = ''
          Where systemd-sysupdate looks for SHA256SUMS and the artifacts it
          lists. Must end in a slash: sysupdate appends file names to it.

          The default is losos-update-source on this machine
          (update-source.nix), which serves the channel's newest GitHub
          release from `losos.update.index`. The flake sets it to the
          proxy's `/updates/<channel>/<arch>/` instead when
          LOSOS_PROXY_URL is set.
        '';
      };

      index = mkOption {
        type = types.nullOr types.str;
        default =
          if config.losos.docs.url == null then
            null
          else
            "${config.losos.docs.url}/updates/{channel}/${config.losos.arch.name}/index";
        defaultText = lib.literalExpression ''"''${config.losos.docs.url}/updates/{channel}/<arch>/index"'';
        description = ''
          Each channel's index on the documentation site
          (website/release-index.js): which GitHub release assets make up
          each file of its newest release, with `{channel}` where the
          channel's name goes. losos-update-source reads the one of the
          channel the machine follows when `baseUrl` is its own address.
        '';
      };

      pubring = mkOption {
        type = types.path;
        # The release key's public half is committed armored, so a change to
        # it reads as text in review. gpg's --keyring, which systemd-pull
        # verifies with, takes only binary keys, hence the dearmor.
        default =
          pkgs.runCommand "import-pubring.gpg" { nativeBuildInputs = [ pkgs.buildPackages.gnupg ]; }
            ''
              export GNUPGHOME=$(mktemp -d)
              gpg --batch --dearmor < ${../keys/update-signing.asc} > $out
            '';
        defaultText = lib.literalMD "`nixos/keys/update-signing.asc`, dearmored";
        description = ''
          The GPG public keyring SHA256SUMS.gpg is verified against. CI signs
          each release's SHA256SUMS with the secret half (docs/binary-cache.md says how the two are made).
        '';
      };
    };

    usrSize = mkOption {
      type = types.str;
      default = "8G";
      description = ''
        The size of each /usr slot once first boot has grown it. An update
        writes a whole new /usr into the slot not currently booted, so this
        is the ceiling on how large the Nix store of any future release may
        be; it cannot be raised later without repartitioning.
      '';
    };

    arch = mkOption {
      type = types.attrsOf types.str;
      readOnly = true;
      internal = true;
      description = ''
        Every per-architecture spelling the image needs, in one place: each
        is easy to leave at its x86 value, and each such mistake builds
        cleanly and fails on hardware.
      '';
    };
  };

  config.losos.arch =
    {
      x86_64 = {
        name = "x86_64";
        usr = "usr-x86-64";
        usrVerity = "usr-x86-64-verity";
        root = "root-x86-64";
      };
      aarch64 = {
        name = "aarch64";
        usr = "usr-arm64";
        usrVerity = "usr-arm64-verity";
        root = "root-arm64";
      };
    }
    .${hostPlatform.parsed.cpu.name}
      or (throw "losos: unsupported architecture ${hostPlatform.parsed.cpu.name}")
    // {
      # GRUB's file names on the ESP and the stub are named after the EFI
      # machine type, which nixpkgs already knows: x64, aa64.
      efi = hostPlatform.efiArch;
    };
}
