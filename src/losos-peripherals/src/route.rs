//! Where a device goes.
//!
//! By the drivers that would claim it, which modprobe names from its
//! interfaces' modaliases, and where those live in the kernel's tree: a
//! driver under drivers/net drives a network card, wherever the device's
//! own descriptors file it. Many Wi-Fi dongles, Ethernet adapters and
//! Bluetooth radios say only "vendor specific", and only the drivers'
//! ID tables know them. Class codes decide only what no driver claims.

use serde::{Deserialize, Serialize};

use crate::usb::Device;

/// A device's destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Route {
    /// A hub: the host configures it so what is behind it enumerates, and
    /// routes those one by one.
    Hub,
    /// Keyboards, mice, game controllers and security keys: HID and
    /// nothing else, which the host keeps so sign-in and the desktop work.
    Host,
    /// Network cards: the network machine.
    Network,
    /// Bluetooth radios: the Bluetooth machine.
    Bluetooth,
    /// Drives, card readers and phones: the files machine, which mounts
    /// them and gives the host their files.
    Storage,
    /// Cameras: a disposable machine of their own.
    Camera,
    /// Sound cards and headsets: a disposable machine of their own.
    Audio,
    /// Anything else: a disposable machine of its own.
    Other,
}

impl Route {
    /// Whether a device on this route gets a disposable machine made for
    /// it, rather than going to one that is always there.
    pub fn disposable(self) -> bool {
        matches!(self, Route::Camera | Route::Audio | Route::Other)
    }
}

const CLASS_AUDIO: u8 = 0x01;
const CLASS_COMM: u8 = 0x02;
const CLASS_HID: u8 = 0x03;
const CLASS_IMAGE: u8 = 0x06;
const CLASS_STORAGE: u8 = 0x08;
const CLASS_CDC_DATA: u8 = 0x0a;
const CLASS_VIDEO: u8 = 0x0e;
const CLASS_WIRELESS: u8 = 0xe0;

/// Decides from the device and the kernel-tree paths of the modules that
/// would drive its interfaces, such as `kernel/drivers/net/usb/r8152.ko.xz`.
///
/// One device can be several things: a headset is sound and HID (its
/// volume buttons), a modem is a network card and a CD of its drivers. The
/// first of network, Bluetooth, storage, camera and sound wins, since each
/// is what the device is for; input (HID, and the joystick drivers that
/// take vendor-specific game pads) decides only when it is all there is.
pub fn route<'a>(device: &Device, modules: impl IntoIterator<Item = &'a str>) -> Route {
    if device.is_hub() {
        return Route::Hub;
    }
    let mut by_driver = None;
    for path in modules {
        let found = if path.contains("/drivers/net/") {
            Route::Network
        } else if path.contains("/drivers/bluetooth/") {
            Route::Bluetooth
        } else if path.contains("/drivers/usb/storage/") {
            Route::Storage
        } else if path.contains("/drivers/media/") {
            Route::Camera
        } else if path.contains("/sound/") {
            Route::Audio
        } else if path.contains("/drivers/hid/") || path.contains("/drivers/input/") {
            Route::Host
        } else {
            continue;
        };
        by_driver = Some(by_driver.map_or(found, |r: Route| first(r, found)));
    }
    if let Some(r) = by_driver {
        return r;
    }
    let classes = || {
        device
            .interfaces
            .iter()
            .map(|i| (i.class, i.subclass, i.protocol))
    };
    let has = |class: u8| classes().any(|(c, _, _)| c == class);
    if classes().any(|c| c == (CLASS_WIRELESS, 0x01, 0x01)) {
        Route::Bluetooth
    } else if has(CLASS_COMM) || has(CLASS_CDC_DATA) {
        Route::Network
    } else if has(CLASS_STORAGE) || has(CLASS_IMAGE) {
        // Still-image devices are cameras and phones speaking PTP or MTP:
        // their photos and files, which the files machine reads.
        Route::Storage
    } else if has(CLASS_VIDEO) {
        Route::Camera
    } else if has(CLASS_AUDIO) {
        Route::Audio
    } else if !device.interfaces.is_empty() && classes().all(|(c, _, _)| c == CLASS_HID) {
        Route::Host
    } else {
        Route::Other
    }
}

/// Of two routes a device's drivers point at, the one it is for.
fn first(a: Route, b: Route) -> Route {
    let rank = |r| match r {
        Route::Network => 0,
        Route::Bluetooth => 1,
        Route::Storage => 2,
        Route::Camera => 3,
        Route::Audio => 4,
        Route::Host => 5,
        Route::Hub | Route::Other => 6,
    };
    if rank(b) < rank(a) {
        b
    } else {
        a
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::Interface;

    fn device(interfaces: &[(u8, u8, u8)]) -> Device {
        Device {
            interfaces: interfaces
                .iter()
                .enumerate()
                .map(|(n, &(class, subclass, protocol))| Interface {
                    number: n as u8,
                    class,
                    subclass,
                    protocol,
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_keyboard_stays() {
        assert_eq!(route(&device(&[(3, 1, 1), (3, 0, 0)]), []), Route::Host);
    }

    #[test]
    fn a_headset_is_sound_not_a_keyboard() {
        let d = device(&[(1, 1, 0), (1, 2, 0), (3, 0, 0)]);
        assert_eq!(
            route(
                &d,
                [
                    "kernel/drivers/hid/usbhid/usbhid.ko.xz",
                    "kernel/sound/usb/snd-usb-audio.ko.xz"
                ]
            ),
            Route::Audio
        );
    }

    #[test]
    fn a_vendor_specific_game_pad_stays() {
        let d = device(&[(0xff, 0x5d, 0x01)]);
        assert_eq!(
            route(&d, ["kernel/drivers/input/joystick/xpad.ko.xz"]),
            Route::Host
        );
    }

    #[test]
    fn a_vendor_specific_dongle_goes_by_its_driver() {
        let d = device(&[(0xff, 0xff, 0xff)]);
        assert_eq!(route(&d, []), Route::Other);
        assert_eq!(
            route(
                &d,
                ["kernel/drivers/net/wireless/mediatek/mt76/mt76x2/mt76x2u.ko.xz"]
            ),
            Route::Network
        );
        assert_eq!(
            route(&d, ["kernel/drivers/bluetooth/btusb.ko.xz"]),
            Route::Bluetooth
        );
    }

    #[test]
    fn a_modem_with_a_driver_cd_is_a_network_card() {
        let d = device(&[(8, 6, 0x50), (2, 2, 0)]);
        assert_eq!(
            route(
                &d,
                [
                    "kernel/drivers/usb/storage/usb-storage.ko.xz",
                    "kernel/drivers/net/usb/cdc_ether.ko.xz"
                ]
            ),
            Route::Network
        );
    }

    #[test]
    fn classes_decide_without_drivers() {
        assert_eq!(route(&device(&[(0xe0, 1, 1)]), []), Route::Bluetooth);
        assert_eq!(route(&device(&[(8, 6, 0x50)]), []), Route::Storage);
        assert_eq!(route(&device(&[(6, 1, 1)]), []), Route::Storage);
        assert_eq!(
            route(&device(&[(0x0e, 1, 0), (0x0e, 2, 0)]), []),
            Route::Camera
        );
        assert_eq!(route(&device(&[(7, 1, 2)]), []), Route::Other);
        assert_eq!(route(&device(&[]), []), Route::Other);
    }

    #[test]
    fn a_hub_is_a_hub() {
        let d = Device {
            class: 9,
            ..device(&[(9, 0, 0)])
        };
        assert_eq!(route(&d, []), Route::Hub);
    }
}
