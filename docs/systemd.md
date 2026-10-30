# Everything systemd, and the exceptions

Which systemd component does which job:

| Job | Component |
|---|---|
| Boot | `systemd-stub` (UKI), `boot-check-no-failures`, and Plymouth for the splash; GRUB is the boot loader, on UEFI and on a legacy BIOS PC |
| Installation | a live ISO runs `derisk installer` as its session, with NetworkManager for Wi-Fi; its backend runs `systemd-repart` for the ESP and slot A, and `systemd-sysupdate` to fill them from the channel |
| First boot | `systemd-repart` in the initrd creates slot B, root, `/home` and swap; `derisk setup` asks for a language, keyboard, time zone, network and the first user, saved through localed, timedated and homed |
| Read-only `/usr` | the Nix store on a dm-verity partition, with its root hash on the UKI's command line; `systemd-sysext` and `systemd-confext` add layers on top |
| Updates | `systemd-sysupdate` with A/B slots, and `fwupd` for firmware |
| Factory reset | `FactoryReset=` on root and `/home`, and systemd's reset Varlink API |
| Accounts | `systemd-homed`, `systemd-userdbd`, `pam_systemd_home` |
| Administration | pm's system service as its own `pm` user, driving units through the system manager's D-Bus API ([pm on LosOS](pm.md#administering-the-machine)) |
| Session | derisk as the display manager, with its lock screen as the login screen, `systemd-logind` seats, and the user manager driving `derisk-session.target` and `graphical-session.target` |
| File search | `derisk-index.service`, a user service started on its socket, `derisk-index.socket`, at idle priority and with home read-only, indexing the names and text of each person's files for the command palette |
| Network | NetworkManager with NixOS's defaults, `systemd-resolved`, `systemd-timesyncd` |
| Memory | `systemd-oomd`, zswap in front of a RAM-sized encrypted swap partition, and GrapheneOS's hardened_malloc as every process's allocator |
| Sleep | `systemd-sleep` suspends on a laptop's lid; nothing hibernates under the locked-down kernel ([Hibernation](hibernation.md)) |

![A machine's life: the installer lays out the disk with systemd-repart and fills slot A with systemd-sysupdate; first boot creates slot B, root, home and swap; derisk setup creates the first account with homectl; homed opens each person's home at login; sysupdate writes the idle slot and boot counting keeps or gives up the new version; a factory reset empties root and home](images/systemd.svg)

A non-systemd component remains only where systemd has no equivalent, or
where NixOS requires one:

- **nsncd.** The `homed` module asserts `services.nscd.enable`. NixOS routes
  NSS through a forwarder so that a libc can find `nss-systemd` in the store,
  and glibc's NSS uses it for that. It is stateless and keeps no record of
  its own about who exists.
- **NetworkManager.** networkd has no Wi-Fi of its own, so it needed
  wpa_supplicant's control socket beside it. And every app that asks whether
  it is online, such as GLib, the portal and Flatpaks, asks NetworkManager.
  It runs with NixOS's defaults and hands DNS to resolved.
- **nftables.** systemd has no packet filter. The pm tree shipped none at
  all. This image keeps NixOS's firewall and opens mDNS and LLMNR for
  resolved.
- **A PAM stack.** The pm tree shipped none, and its `docs/limits.md` said
  nothing in the image could authenticate anyone. NixOS generates one, with
  `pam_systemd_home` in all four management groups. That is what makes a
  login open the home area, instead of succeeding and finding it locked.

In these places the image uses the systemd equivalent where NixOS would
otherwise pick something else:

- pm's system service, which drives systemd as its own user, instead of
  sudo, su or run0;
- resolved's mDNS instead of avahi;
- `systemd-sysusers` instead of the perl user script;
- the `/etc` overlay instead of activation scripts writing files;
- first-boot setup making the first user with homectl, where NixOS would
  have no way to create one at all.
