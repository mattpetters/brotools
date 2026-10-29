//! The 20-byte clear header and the repeating mask over the rest of the file.
//!
//! The rules are `docs/formats/ptx.md`. Applying the mask twice restores the input.

use crate::{Error, Keying, MAX_FILE_BYTES};

pub(crate) struct Opened {
    pub keying: Keying,
    pub little_endian: bool,
    pub bytes: Vec<u8>,
}

pub(crate) fn reveal(file: &[u8]) -> Result<Opened, Error> {
    let (keying, little_endian) = header(file)?;
    let mut bytes = file.to_vec();
    apply(&mut bytes, keying);
    Ok(Opened { keying, little_endian, bytes })
}

pub(crate) fn conceal(plain: &[u8]) -> Result<Vec<u8>, Error> {
    let (keying, _) = header(plain)?;
    let mut bytes = plain.to_vec();
    apply(&mut bytes, keying);
    Ok(bytes)
}

fn header(file: &[u8]) -> Result<(Keying, bool), Error> {
    if file.len() < 20 {
        return Err(Error::new("shorter than the 20-byte header"));
    }
    if file.len() as u64 > MAX_FILE_BYTES {
        return Err(Error::new("larger than 64 MiB"));
    }
    let marked = file[0] == 0x03 || file.get(1..17) == Some(b"0010111100101011");
    if !marked {
        return Err(Error::new("not a Pro Tools session header"));
    }
    let little_endian = match file[0x11] {
        0 => true,
        1 => false,
        _ => return Err(Error::new("unknown endian flag")),
    };
    let keying = match file[0x12] {
        0x01 => Keying::PerByte,
        0x05 => Keying::PerPage,
        _ => return Err(Error::new("unknown mask scheme")),
    };
    Ok((keying, little_endian))
}

fn apply(bytes: &mut [u8], keying: Keying) {
    let stride = match keying {
        Keying::PerByte => stride_for(bytes[0x13], 53, false),
        Keying::PerPage => stride_for(bytes[0x13], 11, true),
    };
    let mut mask = [0u8; 256];
    for (k, byte) in mask.iter_mut().enumerate() {
        *byte = (k as u8).wrapping_mul(stride);
    }
    for (n, byte) in bytes.iter_mut().enumerate().skip(0x14) {
        let index = match keying {
            Keying::PerByte => n & 0xff,
            Keying::PerPage => (n >> 12) & 0xff,
        };
        *byte ^= mask[index];
    }
}

/// `i` in 0..256 with `(i * multiplier) mod 256 == seed`. Scheme `0x05` then negates `i`.
fn stride_for(seed: u8, multiplier: u8, negate: bool) -> u8 {
    let mul = u16::from(multiplier);
    let want = u16::from(seed);
    let found = (0u16..256).find(|i| i.wrapping_mul(mul) & 0xff == want).unwrap_or(0);
    let low = found as u8;
    if negate { low.wrapping_neg() } else { low }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_buf(scheme: u8, seed: u8, endian: u8, len: usize) -> Vec<u8> {
        let mut v = vec![0u8; len];
        v[0] = 0x03;
        v[1..17].copy_from_slice(b"0010111100101011");
        v[0x11] = endian;
        v[0x12] = scheme;
        v[0x13] = seed;
        v
    }

    #[test]
    fn mask_round_trips_both_schemes() {
        for (scheme, seed, len) in [(0x01u8, 0x11u8, 300usize), (0x05, 11, 5000)] {
            let mut plain = header_buf(scheme, seed, 0, len);
            plain[30] = 0x5a;
            plain[4096.min(len - 1)] = 0xab;
            let hidden = conceal(&plain).unwrap();
            let back = reveal(&hidden).unwrap();
            assert_eq!(back.bytes, plain);
            if scheme == 0x05 {
                assert_eq!(hidden[30], plain[30], "page 0 of the page scheme is unmasked");
                assert_ne!(hidden[4096], plain[4096], "the next page is masked");
            } else {
                assert_ne!(hidden[30], plain[30]);
            }
        }
    }

    #[test]
    fn rejects_a_short_or_unknown_header() {
        assert!(reveal(&[0u8; 10]).is_err());
        let mut bad = header_buf(0x05, 1, 0, 20);
        bad[0x12] = 0x09;
        assert!(reveal(&bad).is_err());
        bad[0x12] = 0x05;
        bad[0x11] = 7;
        assert!(reveal(&bad).is_err());
        bad[0x11] = 0;
        bad[0] = 0;
        bad[1] = b'2';
        assert!(reveal(&bad).is_err());
    }
}
