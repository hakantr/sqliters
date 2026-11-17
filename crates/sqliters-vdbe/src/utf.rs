//! Byte conversion from SQLite 3.54.1 utf.c, without SQLITE_REPLACE_INVALID_UTF.
//! This differs from Rust's Unicode replacement rules for malformed input.

const TRANS1: [u8; 64] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25,
    26, 27, 28, 29, 30, 31, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 0, 1, 2, 3, 4, 5,
    6, 7, 0, 1, 2, 3, 0, 1, 0, 0,
];

fn write_utf8(out: &mut Vec<u8>, c: u32) {
    if c < 0x80 {
        out.push(c as u8);
    } else if c < 0x800 {
        out.extend([0xc0 + ((c >> 6) & 31) as u8, 0x80 + (c & 63) as u8]);
    } else if c < 0x10000 {
        out.extend([
            0xe0 + ((c >> 12) & 15) as u8,
            0x80 + ((c >> 6) & 63) as u8,
            0x80 + (c & 63) as u8,
        ]);
    } else {
        out.extend([
            0xf0 + ((c >> 18) & 7) as u8,
            0x80 + ((c >> 12) & 63) as u8,
            0x80 + ((c >> 6) & 63) as u8,
            0x80 + (c & 63) as u8,
        ]);
    }
}

fn write_utf16(out: &mut Vec<u8>, c: u32, enc: u8) {
    let unit = |out: &mut Vec<u8>, c: u16| {
        out.extend(if enc == 2 {
            c.to_le_bytes()
        } else {
            c.to_be_bytes()
        });
    };
    if c <= 0xffff {
        unit(out, c as u16);
    } else {
        // WRITE_UTF16LE/BE retains these masks even for out-of-range code points.
        let low = ((c >> 10) & 0x3f) + (((c - 0x10000) >> 10) & 0xc0);
        let high = 0xd8 + (((c - 0x10000) >> 18) & 3);
        unit(out, (high * 256 + low) as u16);
        unit(out, (0xdc00 + (c & 0x3ff)) as u16);
    }
}

/// sqlite3VdbeMemTranslate's data bytes (terminators are not part of Mem.z).
/// Encodings: 1 UTF-8, 2 UTF-16le, 3 UTF-16be. Uses the VM Vec allocation policy.
pub fn translate_bytes(input: &[u8], src: u8, dst: u8) -> Vec<u8> {
    debug_assert!((1..=3).contains(&src) && (1..=3).contains(&dst));
    if src == dst {
        return input.to_vec();
    }
    if src != 1 && dst != 1 {
        let mut out = input.to_vec();
        for unit in out.as_chunks_mut::<2>().0 {
            unit.swap(0, 1);
        }
        return out;
    }
    let mut out = Vec::with_capacity(input.len().saturating_mul(2));
    let mut i = 0;
    if src == 1 {
        while i < input.len() {
            let mut c = u32::from(input[i]);
            i += 1;
            if c >= 0xc0 {
                c = u32::from(TRANS1[(c - 0xc0) as usize]);
                while i < input.len() && input[i] & 0xc0 == 0x80 {
                    c = (c << 6).wrapping_add(u32::from(input[i] & 63));
                    i += 1;
                }
                // READ_UTF8 in utf.c does not reject noncharacters here.
                if c < 0x80 || c & 0xffff_f800 == 0xd800 {
                    c = 0xfffd;
                }
            }
            write_utf16(&mut out, c, dst);
        }
    } else {
        let end = input.len() & !1;
        let read = |i| {
            let bytes = [input[i], input[i + 1]];
            u32::from(if src == 2 {
                u16::from_le_bytes(bytes)
            } else {
                u16::from_be_bytes(bytes)
            })
        };
        while i < end {
            let mut c = read(i);
            i += 2;
            if (0xd800..0xe000).contains(&c) && i < end {
                let c2 = read(i);
                i += 2;
                c = (c2 & 0x3ff) + ((c & 0x3f) << 10) + (((c & 0x3c0) + 0x40) << 10);
            }
            write_utf8(&mut out, c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::translate_bytes;

    #[test]
    fn c_byte_conversion_edges_are_not_rust_lossy_decoding() {
        assert_eq!(translate_bytes(&[0x80], 1, 2), [0x80, 0]);
        assert_eq!(translate_bytes(&[0xef, 0xbf, 0xbe], 1, 2), [0xfe, 0xff]);
        assert_eq!(translate_bytes(&[0, 0xdc], 2, 1), [0xed, 0xb0, 0x80]);
        assert_eq!(
            translate_bytes(&[0, 0xd8, 0x41, 0], 2, 1),
            [0xf0, 0x90, 0x81, 0x81]
        );
        assert_eq!(
            translate_bytes(&[0xd8, 0, 0, 0x41], 3, 1),
            [0xf0, 0x90, 0x81, 0x81]
        );
        assert_eq!(translate_bytes(&[0x41, 0, 0xff], 2, 3), [0, 0x41, 0xff]);
        assert_eq!(translate_bytes(&[0x41, 0, 0xff], 2, 1), [0x41]);
    }
}
