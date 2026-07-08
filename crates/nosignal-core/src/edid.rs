//! Minimal EDID blob parsing: exactly the identity triple NoSignal needs
//! (PNP vendor id, product code, serial), preferring the monitor descriptor
//! serial *string* over the numeric serial when present — that is what
//! compositors expose and what survives across ports.

use crate::identity::EdidId;

/// Parse an EDID base block (128+ bytes). Returns `None` when the header is
/// invalid — callers treat that as "EDID unreadable" and fall back to
/// connector identity.
pub fn parse(bytes: &[u8]) -> Option<EdidId> {
    const HEADER: [u8; 8] = [0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00];
    if bytes.len() < 128 || bytes[..8] != HEADER {
        return None;
    }

    // Manufacturer id: two bytes, big-endian, three 5-bit letters ('A' = 1).
    let mfg = u16::from_be_bytes([bytes[8], bytes[9]]);
    let letter = |shift: u16| -> char {
        let v = ((mfg >> shift) & 0x1F) as u8;
        if (1..=26).contains(&v) {
            (b'A' + v - 1) as char
        } else {
            '?'
        }
    };
    let vendor: String = [letter(10), letter(5), letter(0)].iter().collect();

    // Product code and numeric serial are little-endian.
    let product = u16::from_le_bytes([bytes[10], bytes[11]]);
    let serial_num = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);

    // Descriptor blocks (4 × 18 bytes at offset 54): type 0xFF holds the
    // serial string, which beats the numeric serial when present.
    let mut serial_string = None;
    for i in 0..4 {
        let d = &bytes[54 + i * 18..54 + (i + 1) * 18];
        if d[0] == 0 && d[1] == 0 && d[3] == 0xFF {
            let text: String = d[5..18]
                .iter()
                .take_while(|&&b| b != 0x0A)
                .map(|&b| b as char)
                .collect();
            let text = text.trim().to_string();
            if !text.is_empty() {
                serial_string = Some(text);
            }
        }
    }

    Some(EdidId {
        vendor,
        product: format!("0x{product:04x}"),
        serial: serial_string.unwrap_or_else(|| serial_num.to_string()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid EDID: header, vendor "DEL", product 0xA0B1,
    /// numeric serial, optional serial-string descriptor.
    fn edid_bytes(serial_desc: Option<&str>) -> Vec<u8> {
        let mut e = vec![0u8; 128];
        e[..8].copy_from_slice(&[0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00]);
        // "DEL": D=4, E=5, L=12 → 0b0_00100_00101_01100
        let mfg: u16 = (4 << 10) | (5 << 5) | 12;
        e[8..10].copy_from_slice(&mfg.to_be_bytes());
        e[10..12].copy_from_slice(&0xA0B1u16.to_le_bytes());
        e[12..16].copy_from_slice(&305419896u32.to_le_bytes());
        if let Some(s) = serial_desc {
            // Descriptor 0 at offset 54: type 0xFF serial string.
            e[54] = 0;
            e[55] = 0;
            e[57] = 0xFF;
            let mut text = s.as_bytes().to_vec();
            text.push(0x0A);
            text.resize(13, 0x20);
            e[59..72].copy_from_slice(&text);
        }
        e
    }

    #[test]
    fn parses_vendor_product_and_numeric_serial() {
        let id = parse(&edid_bytes(None)).unwrap();
        assert_eq!(id.vendor, "DEL");
        assert_eq!(id.product, "0xa0b1");
        assert_eq!(id.serial, "305419896");
    }

    #[test]
    fn serial_string_descriptor_wins() {
        let id = parse(&edid_bytes(Some("ABC123XYZ"))).unwrap();
        assert_eq!(id.serial, "ABC123XYZ");
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse(&[0u8; 16]).is_none());
        assert!(parse(&[7u8; 128]).is_none());
    }
}
