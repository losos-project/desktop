//! Linux key codes, which say where a key is on the keyboard, to macOS's
//! virtual key codes (Carbon's kVK_*), which say the same thing.
//!
//! Both name positions, not characters, so a key lands where it is on the
//! layout macOS has selected. The host's Alt is the Mac's Option and its
//! logo key the Mac's Command, where they sit on a Mac's keyboard; Insert,
//! which a Mac lacks, is Help, as in the place Apple's extended keyboards
//! put it.

use app_windows_protocol::modifiers;

/// The virtual key code for a Linux key code, or `None` for keys macOS has
/// no code for.
pub fn mac_key(code: u32) -> Option<u16> {
    Some(match code {
        1 => 0x35,   // Esc
        2 => 0x12,   // 1
        3 => 0x13,   // 2
        4 => 0x14,   // 3
        5 => 0x15,   // 4
        6 => 0x17,   // 5
        7 => 0x16,   // 6
        8 => 0x1A,   // 7
        9 => 0x1C,   // 8
        10 => 0x19,  // 9
        11 => 0x1D,  // 0
        12 => 0x1B,  // -
        13 => 0x18,  // =
        14 => 0x33,  // Backspace, which macOS calls Delete
        15 => 0x30,  // Tab
        16 => 0x0C,  // Q
        17 => 0x0D,  // W
        18 => 0x0E,  // E
        19 => 0x0F,  // R
        20 => 0x11,  // T
        21 => 0x10,  // Y
        22 => 0x20,  // U
        23 => 0x22,  // I
        24 => 0x1F,  // O
        25 => 0x23,  // P
        26 => 0x21,  // [
        27 => 0x1E,  // ]
        28 => 0x24,  // Enter
        29 => 0x3B,  // Left Ctrl
        30 => 0x00,  // A
        31 => 0x01,  // S
        32 => 0x02,  // D
        33 => 0x03,  // F
        34 => 0x05,  // G
        35 => 0x04,  // H
        36 => 0x26,  // J
        37 => 0x28,  // K
        38 => 0x25,  // L
        39 => 0x29,  // ;
        40 => 0x27,  // '
        41 => 0x32,  // `
        42 => 0x38,  // Left Shift
        43 => 0x2A,  // \
        44 => 0x06,  // Z
        45 => 0x07,  // X
        46 => 0x08,  // C
        47 => 0x09,  // V
        48 => 0x0B,  // B
        49 => 0x2D,  // N
        50 => 0x2E,  // M
        51 => 0x2B,  // ,
        52 => 0x2F,  // .
        53 => 0x2C,  // /
        54 => 0x3C,  // Right Shift
        55 => 0x43,  // Keypad *
        56 => 0x3A,  // Left Alt: Option
        57 => 0x31,  // Space
        58 => 0x39,  // Caps Lock
        59 => 0x7A,  // F1
        60 => 0x78,  // F2
        61 => 0x63,  // F3
        62 => 0x76,  // F4
        63 => 0x60,  // F5
        64 => 0x61,  // F6
        65 => 0x62,  // F7
        66 => 0x64,  // F8
        67 => 0x65,  // F9
        68 => 0x6D,  // F10
        69 => 0x47,  // Num Lock: the keypad's Clear, where a Mac has it
        71 => 0x59,  // Keypad 7
        72 => 0x5B,  // Keypad 8
        73 => 0x5C,  // Keypad 9
        74 => 0x4E,  // Keypad -
        75 => 0x56,  // Keypad 4
        76 => 0x57,  // Keypad 5
        77 => 0x58,  // Keypad 6
        78 => 0x45,  // Keypad +
        79 => 0x53,  // Keypad 1
        80 => 0x54,  // Keypad 2
        81 => 0x55,  // Keypad 3
        82 => 0x52,  // Keypad 0
        83 => 0x41,  // Keypad .
        86 => 0x0A,  // the key beside Left Shift on ISO keyboards
        87 => 0x67,  // F11
        88 => 0x6F,  // F12
        96 => 0x4C,  // Keypad Enter
        97 => 0x3E,  // Right Ctrl
        98 => 0x4B,  // Keypad /
        100 => 0x3D, // Right Alt: Right Option
        102 => 0x73, // Home
        103 => 0x7E, // Up
        104 => 0x74, // Page Up
        105 => 0x7B, // Left
        106 => 0x7C, // Right
        107 => 0x77, // End
        108 => 0x7D, // Down
        109 => 0x79, // Page Down
        110 => 0x72, // Insert: Help
        111 => 0x75, // Delete, which macOS calls Forward Delete
        113 => 0x4A, // Mute
        114 => 0x49, // Volume Down
        115 => 0x48, // Volume Up
        117 => 0x51, // Keypad =
        125 => 0x37, // Left logo key: Command
        126 => 0x36, // Right logo key: Right Command
        183 => 0x69, // F13
        184 => 0x6B, // F14
        185 => 0x71, // F15
        186 => 0x6A, // F16
        187 => 0x40, // F17
        188 => 0x4F, // F18
        189 => 0x50, // F19
        190 => 0x5A, // F20
        _ => return None,
    })
}

/// Whether a Linux key code is a modifier, which macOS takes as a change
/// of flags rather than as a key going down.
pub fn is_modifier(code: u32) -> bool {
    matches!(code, 29 | 42 | 54 | 56 | 58 | 97 | 100 | 125 | 126)
}

/// CGEventFlags' bits for the protocol's modifier bits.
pub fn mac_flags(held: u32) -> u64 {
    let mut flags = 0;
    if held & modifiers::SHIFT != 0 {
        flags |= 0x0002_0000; // kCGEventFlagMaskShift
    }
    if held & modifiers::CONTROL != 0 {
        flags |= 0x0004_0000; // kCGEventFlagMaskControl
    }
    if held & modifiers::ALT != 0 {
        flags |= 0x0008_0000; // kCGEventFlagMaskAlternate
    }
    if held & modifiers::SUPER != 0 {
        flags |= 0x0010_0000; // kCGEventFlagMaskCommand
    }
    if held & modifiers::CAPS_LOCK != 0 {
        flags |= 0x0001_0000; // kCGEventFlagMaskAlphaShift
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_land_where_they_are() {
        // Q W E R T Y along the top row, A S D F along the middle.
        let row: Vec<_> = (16..=21).map(|c| mac_key(c).unwrap()).collect();
        assert_eq!(row, [0x0C, 0x0D, 0x0E, 0x0F, 0x11, 0x10]);
        assert_eq!(mac_key(30), Some(0x00));
        assert_eq!(mac_key(125), Some(0x37));
        assert_eq!(mac_key(999), None);
    }

    #[test]
    fn no_two_keys_share_a_code() {
        let mut seen = std::collections::HashMap::new();
        for code in 0..256 {
            if let Some(mac) = mac_key(code) {
                assert_eq!(seen.insert(mac, code), None, "kVK {mac:#x} twice");
            }
        }
    }

    #[test]
    fn flags() {
        assert_eq!(mac_flags(modifiers::SUPER | modifiers::SHIFT), 0x0012_0000);
    }
}
