# Kicksecure's hardening, as data: the kernel command line, sysctls and
# kernel modules of its security-misc package (github.com/Kicksecure/
# security-misc, etc/default/grub.d/40_*.cfg, usr/lib/sysctl.d/ and
# etc/modprobe.d/30_security-misc_disable.conf as of 2026-09-30), for the
# machines the OS runs parts of itself in. kicksecure.nix applies it to a
# NixOS service machine; a BusyBox one, which has no sysctl.d or modprobe,
# takes the command line and sets the sysctls from its init.
#
# Left out, because a service machine has no use for them: the IOMMU and EFI
# settings (it has neither an IOMMU of its own nor EFI, and the host's
# IOMMU is what keeps it from the rest of memory, kernel-hardening.nix) and
# lockdown and module signatures (it boots the host's kernel, which forces
# both).
{ isx86 }:

{
  kernelParams = [
    # The allocator: no merged slab caches, poisoned and checked frees,
    # memory zeroed on allocation and on free, a shuffled page allocator.
    "slab_nomerge"
    "slab_debug=FZ"
    "init_on_alloc=1"
    "init_on_free=1"
    "page_alloc.shuffle=1"
    "kfence.sample_interval=100"
    # A random kernel stack offset on every system call.
    "randomize_kstack_offset=on"
    "debugfs=off"
    # Writing another process's memory only where ptrace would allow it.
    "proc_mem.force_override=ptrace"
    # Entropy from more than the CPU's or the boot loader's word.
    "random.trust_cpu=off"
    "random.trust_bootloader=off"
    "extra_latent_entropy"
    # Every CPU mitigation the kernel knows, and no SMT, which several of
    # them can only be complete without.
    "mitigations=auto,nosmt"
    "nosmt=force"
  ]
  ++ (
    if isx86 then
      [
        "vsyscall=none"
        "vdso32=0"
        "ia32_emulation=0"
        "cfi=kcfi"
        "spectre_v2=on"
        "spectre_bhi=on"
        "pti=on"
        "spec_store_bypass_disable=on"
        "ssbd=force-on"
        "l1tf=full,force"
        "mds=full,nosmt"
        "tsx=off"
        "tsx_async_abort=full,nosmt"
        "l1d_flush=on"
        "mmio_stale_data=full,nosmt"
        "retbleed=auto,nosmt"
        "gather_data_sampling=force"
        "reg_file_data_sampling=on"
        "indirect_target_selection=force"
      ]
    else
      [ "kpti=1" ]
  );

  sysctl = {
    # What the kernel tells whom.
    "kernel.kptr_restrict" = 2;
    "kernel.dmesg_restrict" = 1;
    "kernel.printk" = "3 3 3 3";
    "kernel.perf_event_paranoid" = 3;
    # Ways into the kernel no service machine needs.
    "kernel.unprivileged_bpf_disabled" = 1;
    "net.core.bpf_jit_harden" = 2;
    "kernel.kexec_load_disabled" = 1;
    "kernel.io_uring_disabled" = 2;
    "kernel.sysrq" = 0;
    "dev.tty.ldisc_autoload" = 0;
    "dev.tty.legacy_tiocsti" = 0;
    "vm.unprivileged_userfaultfd" = 0;
    # No process may read another's memory, root's included.
    "kernel.yama.ptrace_scope" = 3;
    # Restart at once on a panic, which libvirt does with the domain.
    "kernel.panic" = -1;
    "kernel.randomize_va_space" = 2;
    "vm.mmap_min_addr" = 65536;
    "vm.max_map_count" = 1048576;
    "fs.protected_hardlinks" = 1;
    "fs.protected_symlinks" = 1;
    "fs.protected_fifos" = 2;
    "fs.protected_regular" = 2;
    # No core dumps, which hold whatever secrets the process had.
    "kernel.core_pattern" = "|/bin/false";
    "fs.suid_dumpable" = 0;
    "kernel.core_uses_pid" = 1;
    "vm.swappiness" = 1;
    # The network: no redirects, no source routes, no router adverts, no
    # answers to pings, no timestamps that tell uptime.
    "net.ipv4.tcp_syncookies" = 1;
    "net.ipv4.tcp_rfc1337" = 1;
    "net.ipv4.tcp_timestamps" = 0;
    "net.ipv4.tcp_tw_reuse" = 0;
    "net.ipv4.conf.all.rp_filter" = 1;
    "net.ipv4.conf.default.rp_filter" = 1;
    "net.ipv4.conf.all.accept_redirects" = 0;
    "net.ipv4.conf.default.accept_redirects" = 0;
    "net.ipv4.conf.all.send_redirects" = 0;
    "net.ipv4.conf.default.send_redirects" = 0;
    "net.ipv6.conf.all.accept_redirects" = 0;
    "net.ipv6.conf.default.accept_redirects" = 0;
    "net.ipv4.conf.all.shared_media" = 0;
    "net.ipv4.conf.all.arp_filter" = 1;
    "net.ipv4.conf.all.arp_ignore" = 2;
    "net.ipv4.conf.all.drop_gratuitous_arp" = 1;
    "net.ipv4.icmp_echo_ignore_all" = 1;
    "net.ipv6.icmp.echo_ignore_all" = 1;
    "net.ipv4.icmp_ignore_bogus_error_responses" = 1;
    "net.ipv4.conf.all.accept_source_route" = 0;
    "net.ipv4.conf.default.accept_source_route" = 0;
    "net.ipv6.conf.all.accept_source_route" = 0;
    "net.ipv6.conf.default.accept_source_route" = 0;
    "net.ipv6.conf.all.accept_ra" = 0;
    "net.ipv6.conf.default.accept_ra" = 0;
    "net.ipv4.conf.all.log_martians" = 1;
    "net.ipv4.conf.default.log_martians" = 1;
  };

  # Modules that never load, by name, grouped as security-misc groups them:
  # rare network protocols, rare and network file systems, FireWire,
  # Thunderbolt, GPS, Intel's telemetry, framebuffers and the rest. A
  # machine that needs one, such as a printer machine's, takes it back
  # with losos.service.kicksecure.allowModules.
  disabledModules = {
    network = [
      "af_802154"
      "appletalk"
      "ax25"
      "decnet"
      "dccp"
      "econet"
      "eepro100"
      "eth1394"
      "ipx"
      "n-hdlc"
      "netrom"
      "p8022"
      "p8023"
      "psnap"
      "rose"
      "x25"
      "atm"
      "ueagle-atm"
      "usbatm"
      "xusbatm"
      "c_can"
      "c_can_pci"
      "c_can_platform"
      "can"
      "can-bcm"
      "can-dev"
      "can-gw"
      "can-isotp"
      "can-raw"
      "can-j1939"
      "can327"
      "ifi_canfd"
      "janz-ican3"
      "m_can"
      "m_can_pci"
      "m_can_platform"
      "phy-can-transceiver"
      "slcan"
      "ucan"
      "vxcan"
      "vcan"
      "tipc"
      "tipc_diag"
      "rds"
      "rds_rdma"
      "rds_tcp"
      "sctp"
      "sctp_diag"
    ];
    filesystems = [
      "adfs"
      "affs"
      "afs"
      "befs"
      "ceph"
      "coda"
      "cramfs"
      "ecryptfs"
      "freevxfs"
      "hfs"
      "hfsplus"
      "jffs2"
      "jfs"
      "kafs"
      "minix"
      "nilfs2"
      "ocfs2"
      "orangefs"
      "reiserfs"
      "romfs"
      "sysv"
      "ubifs"
      "udf"
      "ufs"
      "zonefs"
    ];
    networkFilesystems = [
      "gfs2"
      "ksmbd"
      "cifs"
      "cifs_arc4"
      "cifs_md4"
      "nfs"
      "nfs_acl"
      "nfs_layout_nfsv41_files"
      "nfs_layout_flexfiles"
      "nfsd"
      "nfsv2"
      "nfsv3"
      "nfsv4"
    ];
    firewire = [
      "dv1394"
      "firewire-core"
      "firewire-ohci"
      "firewire-net"
      "firewire-sbp2"
      "ohci1394"
      "raw1394"
      "sbp2"
      "video1394"
    ];
    thunderbolt = [
      "intel-wmi-thunderbolt"
      "thunderbolt"
      "thunderbolt_net"
    ];
    gps = [
      "garmin_gps"
      "gnss"
      "gnss-mtk"
      "gnss-serial"
      "gnss-sirf"
      "gnss-ubx"
      "gnss-usb"
    ];
    intelPmt = [
      "pmt_class"
      "pmt_crashlog"
      "pmt_telemetry"
    ];
    framebuffers = [
      "aty128fb"
      "atyfb"
      "cirrusfb"
      "cyber2000fb"
      "cyblafb"
      "gx1fb"
      "hgafb"
      "i810fb"
      "intelfb"
      "kyrofb"
      "lxfb"
      "matroxfb_base"
      "neofb"
      "nvidiafb"
      "pm2fb"
      "radeonfb"
      "rivafb"
      "s1d13xxxfb"
      "savagefb"
      "sisfb"
      "sstfb"
      "tdfxfb"
      "tridentfb"
      "vesafb"
      "vfb"
      "viafb"
      "vt8623fb"
      "udlfb"
    ];
    miscellaneous = [
      "hamradio"
      "floppy"
      "joydev"
      "asus_acpi"
      "bcm43xx"
      "brcm80211"
      "de4x5"
      "prism54"
      "rndis_host"
      "usb_f_rndis"
    ];
  };

  # Not loaded unless asked for by name.
  blacklistedModules = [
    "cdrom"
    "sr_mod"
    "amd76x_edac"
    "ath_pci"
    "evbug"
    "pcspkr"
    "snd_aw2"
    "snd_intel8x0m"
    "snd_pcsp"
    "usbkbd"
    "usbmouse"
  ];
}
