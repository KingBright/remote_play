/// `InputEvent::Key` carries macOS physical key codes on the current wire protocol.
/// Never reinterpret an unmapped code as a Windows virtual key (A is code zero).
pub fn mac_key_to_vk(code: u16) -> Option<u16> {
    Some(match code {
        0x00 => 0x41, // A
        0x01 => 0x53, // S
        0x02 => 0x44, // D
        0x03 => 0x46, // F
        0x04 => 0x48, // H
        0x05 => 0x47, // G
        0x06 => 0x5A, // Z
        0x07 => 0x58, // X
        0x08 => 0x43, // C
        0x09 => 0x56, // V
        0x0B => 0x42, // B
        0x0C => 0x51, // Q
        0x0D => 0x57, // W
        0x0E => 0x45, // E
        0x0F => 0x52, // R
        0x10 => 0x59, // Y
        0x11 => 0x54, // T
        0x12 => 0x31, // 1
        0x13 => 0x32, // 2
        0x14 => 0x33, // 3
        0x15 => 0x34, // 4
        0x16 => 0x36, // 6
        0x17 => 0x35, // 5
        0x18 => 0xBB, // =
        0x19 => 0x39, // 9
        0x1A => 0x37, // 7
        0x1B => 0xBD, // -
        0x1C => 0x38, // 8
        0x1D => 0x30, // 0
        0x1E => 0xDD, // ]
        0x1F => 0x4F, // O
        0x20 => 0x55, // U
        0x21 => 0xDB, // [
        0x22 => 0x49, // I
        0x23 => 0x50, // P
        0x24 => 0x0D, // Return
        0x25 => 0x4C, // L
        0x26 => 0x4A, // J
        0x27 => 0xDE, // '
        0x28 => 0x4B, // K
        0x29 => 0xBA, // ;
        0x2A => 0xDC, // \
        0x2B => 0xBC, // ,
        0x2C => 0xBF, // /
        0x2D => 0x4E, // N
        0x2E => 0x4D, // M
        0x2F => 0xBE, // .
        0x30 => 0x09, // Tab
        0x31 => 0x20, // Space
        0x32 => 0xC0, // `
        0x33 => 0x08, // Backspace
        0x35 => 0x1B, // Escape
        0x36 => 0x5C, // Right Meta
        0x37 => 0x5B, // Left Meta
        0x38 => 0xA0, // Left Shift
        0x39 => 0x14, // Caps Lock
        0x3A => 0xA4, // Left Alt
        0x3B => 0xA2, // Left Control
        0x3C => 0xA1, // Right Shift
        0x3D => 0xA5, // Right Alt
        0x3E => 0xA3, // Right Control
        0x60 => 0x74, // F5
        0x61 => 0x75, // F6
        0x62 => 0x76, // F7
        0x63 => 0x72, // F3
        0x64 => 0x77, // F8
        0x65 => 0x78, // F9
        0x67 => 0x7A, // F11
        0x6D => 0x79, // F10
        0x6F => 0x7B, // F12
        0x72 => 0x2D, // Insert
        0x73 => 0x24, // Home
        0x74 => 0x21, // Page Up
        0x75 => 0x2E, // Delete
        0x76 => 0x73, // F4
        0x77 => 0x23, // End
        0x78 => 0x71, // F2
        0x79 => 0x22, // Page Down
        0x7A => 0x70, // F1
        0x7B => 0x25, // Left
        0x7C => 0x27, // Right
        0x7D => 0x28, // Down
        0x7E => 0x26, // Up
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_letter_and_digit_keys_do_not_become_virtual_key_codes() {
        let letters = [
            0, 11, 8, 2, 14, 3, 5, 4, 34, 38, 40, 37, 46, 45, 31, 35, 12, 15, 1, 17, 32, 9, 13, 7,
            16, 6,
        ];
        for (code, expected) in letters.into_iter().zip(b'A'..=b'Z') {
            assert_eq!(mac_key_to_vk(code), Some(u16::from(expected)));
        }
        for (code, expected) in [29, 18, 19, 20, 21, 23, 22, 26, 28, 25]
            .into_iter()
            .zip(b'0'..=b'9')
        {
            assert_eq!(mac_key_to_vk(code), Some(u16::from(expected)));
        }
    }

    #[test]
    fn navigation_modifiers_and_unknown_keys_are_explicit() {
        assert_eq!(mac_key_to_vk(49), Some(0x20));
        assert_eq!(mac_key_to_vk(59), Some(0xA2));
        assert_eq!(mac_key_to_vk(117), Some(0x2E));
        assert_eq!(mac_key_to_vk(63), None); // Fn has no Windows virtual key.
        assert_eq!(mac_key_to_vk(0xFF), None);
        assert_eq!(mac_key_to_vk(u16::MAX), None);
    }
}
