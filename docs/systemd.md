# Everything systemd, and the exceptions

Which systemd component does each job, and where LosOS uses something else.

| Job | Component |
|---|---|
| Boot | `systemd-stub` (UKI), `boot-check-no-failures`, and Plymouth for the splash; Limine is the boot loader, on UEFI and on a legacy BIOS PC |
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

A non-systemd component stays only where systemd has no equivalent or NixOS
requires one:

- nsncd. NixOS's `homed` module asserts `services.nscd.enable`, so that glibc
  can find `nss-systemd` in the store through a forwarder. nsncd is stateless
  and keeps no record of who exists.
- NetworkManager. networkd has no Wi-Fi of its own and would need
  wpa_supplicant next to it. Apps that ask whether they are online, such as
  GLib, the portal and Flatpaks, ask NetworkManager. It runs with NixOS's
  defaults and hands DNS to resolved.
- nftables. systemd has no packet filter. The image keeps NixOS's firewall and
  opens mDNS and LLMNR for resolved.
- A PAM stack. NixOS generates one, with `pam_systemd_home` in all four
  management groups, so a login unlocks the home instead of finding it locked.
  The old pm tree had no PAM, so nothing in that image could authenticate
  anyone.

In these places the image picks the systemd tool where NixOS would pick
something else:

- pm's system service, driving systemd as its own user, instead of sudo, su or run0.
- resolved's mDNS instead of avahi.
- `systemd-sysusers` instead of the Perl user script.
- The `/etc` overlay instead of activation scripts writing files.
- First-boot setup creating the first user with homectl. NixOS has no way to do that at all.
