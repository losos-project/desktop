# Secure Boot on a PC (docs/secure-boot.md): with LosOS's key, GRUB and every
# UKI are signed, and shim, signed by Microsoft, starts GRUB once a person
# has enrolled LosOS's certificate in MokManager. The ESP's files are
# shim.nix's, put there by grub.nix; this signs the UKI, checks the key and
# certificate belong together, and lets Settings ask shim to enroll the
# certificate, so a person meets MokManager expecting it, with a code to
# type, instead of a "Security Violation".
#
# The key comes from outside the Nix store (options.nix) because the store
# is world-readable and the binary cache publishes it. A derivation carries
# only its path, which shim.nix's signing step reads outside the sandbox on
# a builder that has it.
{
  config,
  lib,
  pkgs,
  ...
}:

let
  cfg = config.losos.secureBoot;
  shim = import ./shim.nix {
    inherit pkgs;
    inherit (cfg) privateKey certificate;
  };

  # Where the enrollment unit leaves the code MokManager will ask for, for
  # Settings to show. Readable by everyone: the code proves only that
  # someone at the keyboard agreed, and that someone is the one reading it.
  run = "/run/losos-secure-boot";
  efivar = name: guid: "/sys/firmware/efi/efivars/${name}-${guid}";
  secureBootVar = efivar "SecureBoot" "8be4df61-93ca-11d2-aa0d-00e098032b8c";

  # `losos-secure-boot` prints where Secure Boot stands, as one line of
  # JSON for Settings (derisk's System page): whether the firmware has it
  # on, whether LosOS's certificate is in the MOK list or waiting to be,
  # and the code MokManager will ask for. `losos-secure-boot enroll`, as
  # root, asks shim to enroll the certificate at the next start, and
  # `cancel` takes the request back; Settings runs them through
  # losos-secure-boot-{enroll,cancel}.service, so polkit asks an
  # administrator first.
  command = pkgs.writeShellApplication {
    name = "losos-secure-boot";
    runtimeInputs = [
      pkgs.coreutils
      pkgs.gnugrep
      pkgs.mokutil
    ];
    text = ''
      cert=${shim.certificateDer}
      case ''${1-} in
        "")
          on=false
          [ "$(od -An -tu1 -j4 -N1 ${secureBootVar} 2> /dev/null | tr -d ' ')" = 1 ] && on=true
          # mokutil reads shim's copies of the lists, MokListRT and
          # MokNew, which any user may. --ignore-keyring, or a key the
          # kernel loaded would answer that instead.
          state=$(mokutil --ignore-keyring --test-key "$cert" 2> /dev/null || true)
          key=missing
          case $state in
            *"already enrolled"*) key=enrolled ;;
            *"enrollment request"*) key=requested ;;
          esac
          code=null
          if [ "$key" = requested ] && read -r digits 2> /dev/null < ${run}/code; then
            code="\"$digits\""
          fi
          printf '{"secure_boot":%s,"key":"%s","code":%s}\n' "$on" "$key" "$code"
          ;;
        enroll)
          install -d -m 0755 ${run}
          # Eight digits: MokManager reads the keyboard as US English, and
          # digits are where every layout agrees.
          code=$(printf '%08d' "$(shuf --input-range=0-99999999 --head-count=1 --random-source=/dev/urandom)")
          # A request left from before would keep its own code, which no
          # one has any more.
          mokutil --revoke-import > /dev/null 2>&1 || true
          umask 077
          mokutil --generate-hash="$code" > ${run}/hash
          mokutil --ignore-keyring --import "$cert" --hash-file ${run}/hash
          rm -f ${run}/hash
          # MokManager waits for a key press instead of starting LosOS after
          # ten seconds, which is easy to miss.
          mokutil --timeout -1
          umask 022
          echo "$code" > ${run}/code.new
          mv ${run}/code.new ${run}/code
          ;;
        cancel)
          mokutil --revoke-import > /dev/null 2>&1 || true
          mokutil --timeout 10
          rm -f ${run}/code
          ;;
        *)
          echo "usage: losos-secure-boot [enroll|cancel]" >&2
          exit 2
          ;;
      esac
    '';
  };
in
{
  system.build.secureBoot = shim;

  environment.systemPackages = lib.mkIf shim.enable [ command ];

  systemd.services.losos-secure-boot-enroll = lib.mkIf shim.enable {
    description = "Ask shim to enroll LosOS's Secure Boot key at the next start";
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${lib.getExe command} enroll";
    };
  };
  systemd.services.losos-secure-boot-cancel = lib.mkIf shim.enable {
    description = "Take back the request to enroll LosOS's Secure Boot key";
    serviceConfig = {
      Type = "oneshot";
      ExecStart = "${lib.getExe command} cancel";
    };
  };

  # A key without its certificate fails in shim.nix, where the installer's
  # medium meets it too.
  assertions = [
    {
      # A key in the store is in the binary cache. The tests sign with one
      # that is public on purpose, which they say by putting it there.
      assertion =
        cfg.privateKey != null && lib.hasPrefix builtins.storeDir cfg.privateKey
        -> lib.hasInfix "losos-secure-boot-test-key" cfg.privateKey;
      message = "losos.secureBoot.privateKey is in the Nix store, where anyone can read it. Give the build a path outside it (docs/secure-boot.md).";
    }
  ];

  # nixpkgs' UKI with usrhash= on its command line (repart-verity-store.nix),
  # built the same way and then signed. ukify can sign as it builds, but
  # then the step that reads the key unsandboxed would be the one that
  # reads the kernel, the initrd and the rest too; the signing step reads
  # one file (shim.nix). The kernel inside is left unsigned: shim accepts it
  # as part of the UKI it has already checked.
  system.build.uki = lib.mkIf shim.enable (
    lib.mkOverride 98 (
      let
        inherit (config.system.boot.loader) ukiFile;
        cmdline = "init=${config.system.build.toplevel}/init ${toString config.boot.kernelParams}";
        unsigned =
          pkgs.runCommand "${ukiFile}-unsigned"
            {
              nativeBuildInputs = [
                pkgs.buildPackages.jq
                pkgs.buildPackages.systemdUkify
              ];
            }
            ''
              usrhash=$(jq -r '.[] | select(.type=="${config.losos.arch.usrVerity}") | .roothash' \
                ${config.system.build.intermediateImage}/repart-output.json)
              ukify build --config=${config.boot.uki.configFile} \
                --cmdline="${cmdline} usrhash=$usrhash" --output=$out
            '';
      in
      pkgs.runCommand ukiFile { } ''
        mkdir $out
        cp ${shim.sign ukiFile unsigned} $out/${ukiFile}
      ''
    )
  );
}
