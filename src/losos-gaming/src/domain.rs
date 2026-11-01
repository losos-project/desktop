//! The gaming machine's libvirt definition.
//!
//! A Windows PC as close to the person's own as libvirt makes it, because
//! the games it is for check: their anti-cheat looks for signs of a virtual
//! machine and some refuse to start in one (docs/gaming-machine.md says
//! which signs remain). So KVM hides its signature, Hyper-V's vendor ID is
//! not KVM's, the firmware tables carry the host's own maker, model and
//! serial, the processor is the host's own, and no device is a virtio one,
//! whose Red Hat vendor ID names the hypervisor: the disk is SATA, the
//! network card an Intel e1000e and the sound Intel HDA, all of which
//! Windows drives without a drivers CD. Secure Boot with Microsoft's keys
//! and a TPM are on, which Windows 11 and several anti-cheats require.
//!
//! The picture comes out of the card: on a screen plugged into it, or
//! through Looking Glass, which copies each frame the card draws into
//! memory the host shares with the machine (an IVSHMEM device over
//! /dev/shm/looking-glass) and shows it in a window on the desktop.

use std::fmt::Write;

/// The domain's name, and what `virsh` calls it.
pub const NAME: &str = "gaming";
/// Its disk, a qcow2 volume in libvirt's `default` pool.
pub const DISK: &str = "losos-gaming.qcow2";
/// The XML namespace of what this program records in its `<metadata>`.
pub const NAMESPACE: &str = "https://losos-project.github.io/xmlns/gaming-machine/1";
/// OVMF built for Secure Boot, and the variables it starts from, with
/// Microsoft's keys enrolled (gaming.nix links both from nixpkgs'
/// OVMFFull). Named here rather than chosen by libvirt from QEMU's own
/// firmware, none of which has the keys Windows' boot loader is signed
/// with.
pub const FIRMWARE: &str = "/etc/losos-gaming/OVMF_CODE.fd";
pub const VARIABLES: &str = "/etc/losos-gaming/OVMF_VARS.ms.fd";
/// Looking Glass's shared memory, which gaming.nix makes for the
/// administrators who may run the machine. 128 MiB holds two frames of 4K
/// at 32 bits.
pub const SHARED: &str = "/dev/shm/looking-glass";
pub const SHARED_MIB: u32 = 128;

/// Where the machine's picture goes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Screen {
    /// A window on the desktop, through Looking Glass, whose client sends
    /// the keyboard and pointer back over SPICE.
    LookingGlass,
    /// A screen plugged into the card. The keyboard and pointer go to the
    /// machine through evdev, and both Ctrl keys at once bring them back.
    Monitor,
}

impl Screen {
    pub fn name(self) -> &'static str {
        match self {
            Screen::LookingGlass => "looking-glass",
            Screen::Monitor => "monitor",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "looking-glass" => Some(Screen::LookingGlass),
            "monitor" => Some(Screen::Monitor),
            _ => None,
        }
    }
}

/// Everything the definition depends on.
#[derive(Clone, Debug)]
pub struct Spec {
    pub memory_mib: u64,
    pub cores: u32,
    /// Threads per core: 2 where the host has SMT, so Windows schedules
    /// the pairs the way it would on the bare processor.
    pub threads: u32,
    /// The host's CPU is AMD's, whose SMT topology QEMU passes only with
    /// `topoext` asked for.
    pub amd: bool,
    /// The card's PCI functions, already on vfio-pci.
    pub functions: Vec<String>,
    /// USB game pads, `(vendor, product)`, handed over as the machine
    /// starts.
    pub pads: Vec<(u16, u16)>,
    /// With [`Screen::Monitor`], the keyboards' and pointers' event
    /// devices, by-id.
    pub inputs: Vec<Input>,
    pub screen: Screen,
    /// Until Windows has the card's driver and Looking Glass's host, the
    /// machine also has an emulated screen to install them on.
    pub installing: bool,
    /// The CD volumes Windows installs from, while installing.
    pub media: Vec<String>,
}

/// An event device handed to the machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Input {
    pub path: String,
    /// Keyboards carry the grab toggle, and the repeat QEMU must not add
    /// to Windows' own.
    pub keyboard: bool,
}

/// The domain's XML.
pub fn xml(spec: &Spec) -> String {
    let mut devices = String::new();
    let _ = write!(
        devices,
        "
    <disk type='volume' device='disk'>
      <driver name='qemu' type='qcow2' cache='none' io='native' discard='unmap'/>
      <source pool='default' volume='{DISK}'/>
      <target dev='sda' bus='sata'/>
      <boot order='1'/>
    </disk>"
    );
    if spec.installing {
        for (i, volume) in spec.media.iter().enumerate() {
            let letter = char::from(b'b' + u8::try_from(i).unwrap_or(24).min(24));
            let boot = if i == 0 {
                "\n      <boot order='2'/>"
            } else {
                ""
            };
            let _ = write!(
                devices,
                "
    <disk type='volume' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source pool='default' volume='{}'/>
      <target dev='sd{letter}' bus='sata'/>
      <readonly/>{boot}
    </disk>",
                escape(volume)
            );
        }
    }
    devices.push_str(
        "
    <interface type='network'>
      <source network='default'/>
      <model type='e1000e'/>
    </interface>
    <tpm model='tpm-crb'>
      <backend type='emulator' version='2.0'/>
    </tpm>",
    );
    for f in &spec.functions {
        let Some((domain, bus, slot, function)) = split(f) else {
            continue;
        };
        // Not managed: gaming.nix's boot unit has the card on vfio-pci
        // already, and libvirt must not hand it back to a host driver when
        // the machine stops.
        let _ = write!(
            devices,
            "
    <hostdev mode='subsystem' type='pci' managed='no'>
      <source>
        <address domain='0x{domain}' bus='0x{bus}' slot='0x{slot}' function='0x{function}'/>
      </source>
    </hostdev>"
        );
    }
    if !spec.pads.is_empty() {
        devices.push_str("\n    <controller type='usb' model='qemu-xhci'/>");
    }
    for (vendor, product) in &spec.pads {
        let _ = write!(
            devices,
            "
    <hostdev mode='subsystem' type='usb' managed='yes'>
      <source startupPolicy='optional'>
        <vendor id='0x{vendor:04x}'/>
        <product id='0x{product:04x}'/>
      </source>
    </hostdev>"
        );
    }
    match spec.screen {
        Screen::LookingGlass => {
            // The client's keyboard, pointer and sound: SPICE on the
            // loopback only, with its own picture, which has nothing to
            // show, uncompressed.
            let _ = write!(
                devices,
                "
    <graphics type='spice' autoport='yes'>
      <listen type='address' address='127.0.0.1'/>
      <image compression='off'/>
    </graphics>
    <sound model='ich9'>
      <audio id='1'/>
    </sound>
    <audio id='1' type='spice'/>
    <shmem name='looking-glass'>
      <model type='ivshmem-plain'/>
      <size unit='M'>{SHARED_MIB}</size>
    </shmem>"
            );
        }
        Screen::Monitor => {
            for input in &spec.inputs {
                let grab = if input.keyboard {
                    " grab='all' grabToggle='ctrl-ctrl' repeat='on'"
                } else {
                    ""
                };
                let _ = write!(
                    devices,
                    "
    <input type='evdev'>
      <source dev='{}'{grab}/>
    </input>",
                    escape(&input.path)
                );
            }
        }
    }
    let video = if spec.installing { "vga" } else { "none" };
    let _ = write!(
        devices,
        "
    <video>
      <model type='{video}'/>
    </video>
    <memballoon model='none'/>"
    );

    let topoext = if spec.amd && spec.threads > 1 {
        "\n    <feature policy='require' name='topoext'/>"
    } else {
        ""
    };
    let vcpus = spec.cores * spec.threads;
    format!(
        "<domain type='kvm'>
  <name>{NAME}</name>
  <title>Gaming</title>
  <metadata>
    <pm:machine xmlns:pm='https://losos-project.github.io/xmlns/pm-machine/1'>
      <pm:label>red</pm:label>
      <pm:system>windows</pm:system>
    </pm:machine>
    <gaming:machine xmlns:gaming='{NAMESPACE}' screen='{screen}' installing='{installing}'/>
  </metadata>
  <memory unit='MiB'>{memory}</memory>
  <vcpu placement='static'>{vcpus}</vcpu>
  <os>
    <type arch='x86_64' machine='q35'>hvm</type>
    <loader readonly='yes' secure='yes' type='pflash'>{FIRMWARE}</loader>
    <nvram template='{VARIABLES}'/>
    <smbios mode='host'/>
  </os>
  <features>
    <acpi/>
    <apic/>
    <hyperv mode='custom'>
      <relaxed state='on'/>
      <vapic state='on'/>
      <spinlocks state='on' retries='8191'/>
      <vpindex state='on'/>
      <runtime state='on'/>
      <synic state='on'/>
      <stimer state='on'/>
      <frequencies state='on'/>
      <tlbflush state='on'/>
      <ipi state='on'/>
      <reset state='on'/>
      <vendor_id state='on' value='LososGaming'/>
    </hyperv>
    <kvm>
      <hidden state='on'/>
    </kvm>
    <vmport state='off'/>
    <smm state='on'/>
  </features>
  <cpu mode='host-passthrough' check='none' migratable='off'>
    <topology sockets='1' dies='1' cores='{cores}' threads='{threads}'/>
    <cache mode='passthrough'/>{topoext}
  </cpu>
  <clock offset='localtime'>
    <timer name='hypervclock' present='yes'/>
    <timer name='rtc' tickpolicy='catchup'/>
    <timer name='pit' tickpolicy='delay'/>
    <timer name='hpet' present='no'/>
  </clock>
  <on_poweroff>destroy</on_poweroff>
  <on_reboot>restart</on_reboot>
  <on_crash>destroy</on_crash>
  <pm>
    <suspend-to-mem enabled='no'/>
    <suspend-to-disk enabled='no'/>
  </pm>
  <devices>{devices}
  </devices>
</domain>
",
        screen = spec.screen.name(),
        installing = if spec.installing { "yes" } else { "no" },
        memory = spec.memory_mib,
        cores = spec.cores,
        threads = spec.threads,
    )
}

/// What a previous definition recorded: its screen and whether it is
/// still installing, from `virsh metadata gaming <NAMESPACE>`.
pub fn recorded(metadata: &str) -> (Option<Screen>, Option<bool>) {
    let attr = |name: &str| {
        let start = metadata
            .find(&format!("{name}='"))
            .map(|i| i + name.len() + 2)
            .or_else(|| {
                metadata
                    .find(&format!("{name}=\""))
                    .map(|i| i + name.len() + 2)
            })?;
        let rest = &metadata[start..];
        Some(&rest[..rest.find(['\'', '"'])?])
    };
    (
        attr("screen").and_then(Screen::parse),
        attr("installing").map(|v| v == "yes"),
    )
}

/// `0000:01:00.1` as libvirt's four hex fields.
fn split(address: &str) -> Option<(&str, &str, &str, &str)> {
    let (domain, rest) = address.split_once(':')?;
    let (bus, rest) = rest.split_once(':')?;
    let (slot, function) = rest.split_once('.')?;
    Some((domain, bus, slot, function))
}

/// Text for an XML attribute.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('\'', "&apos;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(screen: Screen, installing: bool) -> Spec {
        Spec {
            memory_mib: 16384,
            cores: 6,
            threads: 2,
            amd: true,
            functions: vec!["0000:01:00.0".into(), "0000:01:00.1".into()],
            pads: vec![(0x045e, 0x0b12)],
            inputs: vec![Input {
                path: "/dev/input/by-id/usb-kbd-event-kbd".into(),
                keyboard: true,
            }],
            screen,
            installing,
            media: vec!["losos-gaming-windows.iso".into()],
        }
    }

    #[test]
    fn the_machine_does_not_say_it_is_one() {
        let xml = xml(&spec(Screen::LookingGlass, false));
        for wanted in [
            "<hidden state='on'/>",
            "<vendor_id state='on' value='LososGaming'/>",
            "<smbios mode='host'/>",
            "mode='host-passthrough'",
            "OVMF_VARS.ms.fd",
            "<memballoon model='none'/>",
            "<model type='e1000e'/>",
            "bus='sata'",
            "name='topoext'",
            "<topology sockets='1' dies='1' cores='6' threads='2'/>",
        ] {
            assert!(xml.contains(wanted), "{wanted} missing:\n{xml}");
        }
        assert!(!xml.contains("virtio"), "{xml}");
        assert!(!xml.contains("cdrom"), "{xml}");
    }

    #[test]
    fn the_card_goes_unmanaged_and_pads_with_it() {
        let xml = xml(&spec(Screen::LookingGlass, false));
        assert!(xml.contains("managed='no'"));
        assert!(xml.contains("bus='0x01' slot='0x00' function='0x1'"));
        assert!(xml.contains("<vendor id='0x045e'/>"));
    }

    #[test]
    fn looking_glass_gets_its_memory_and_spice_and_a_monitor_gets_evdev() {
        let lg = xml(&spec(Screen::LookingGlass, false));
        assert!(
            lg.contains("<shmem name='looking-glass'>")
                && lg.contains("type='spice'")
                && !lg.contains("evdev")
        );
        let monitor = xml(&spec(Screen::Monitor, false));
        assert!(!monitor.contains("shmem") && !monitor.contains("spice"));
        assert!(monitor.contains("grabToggle='ctrl-ctrl'"));
    }

    #[test]
    fn installing_adds_a_screen_and_the_cd() {
        let xml = xml(&spec(Screen::LookingGlass, true));
        assert!(xml.contains("<model type='vga'/>"));
        assert!(xml.contains("volume='losos-gaming-windows.iso'"));
        assert!(xml.contains("installing='yes'"));
    }

    #[test]
    fn what_was_recorded_is_read_back() {
        let xml = xml(&spec(Screen::Monitor, true));
        let start = xml.find("<gaming:machine").unwrap();
        let end = start + xml[start..].find("/>").unwrap() + 2;
        assert_eq!(
            recorded(&xml[start..end]),
            (Some(Screen::Monitor), Some(true))
        );
    }
}
