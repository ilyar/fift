use everscale_types::cell::MAX_BIT_LEN;
use everscale_types::prelude::*;
use num_bigint::BigInt;
use num_traits::Num;
use unicode_segmentation::UnicodeSegmentation;

use crate::error::*;

pub struct ImmediateInt {
    pub num: BigInt,
    pub denom: Option<BigInt>,
}

impl ImmediateInt {
    pub fn try_from_str(s: &str) -> Result<Option<Self>> {
        let (num, denom) = if let Some((left, right)) = s.split_once('/') {
            let Some(num) = Self::parse_single_number(left)? else {
                return Ok(None);
            };
            let Some(denom) = Self::parse_single_number(right)? else {
                return Err(Error::InvalidNumber);
            };
            (num, Some(denom))
        } else {
            let Some(num) = Self::parse_single_number(s)? else {
                return Ok(None);
            };
            (num, None)
        };
        Ok(Some(ImmediateInt { num, denom }))
    }

    fn parse_single_number(s: &str) -> Result<Option<BigInt>> {
        let (neg, s) = match s.strip_prefix('-') {
            Some(s) => (true, s),
            None => (false, s),
        };

        let mut num = if let Some(s) = s.strip_prefix("0x") {
            BigInt::from_str_radix(s, 16)
        } else if let Some(s) = s.strip_prefix("0b") {
            BigInt::from_str_radix(s, 2)
        } else {
            if !s.chars().all(|c| c.is_ascii_digit()) {
                return Ok(None);
            }
            BigInt::from_str_radix(s, 10)
        }
        .map_err(|_| Error::InvalidNumber)?;

        if neg {
            num = -num;
        }

        Ok(Some(num))
    }
}

pub(crate) fn reverse_utf8_string_inplace(s: &mut str) {
    unsafe {
        let v = s.as_bytes_mut();

        // Reverse the bytes within each grapheme cluster.
        // This does not preserve UTF-8 validity.
        {
            // Invariant: `tail` points to data we have not modified yet, so it is always valid UTF-8.
            let mut tail = &mut v[..];
            while let Some(len) = std::str::from_utf8_unchecked(tail)
                .graphemes(true)
                .next()
                .map(str::len)
            {
                let (grapheme, new_tail) = tail.split_at_mut(len);
                grapheme.reverse();
                tail = new_tail;
            }
        }

        // Reverse all bytes. This restores multi-byte sequences to their original order.
        v.reverse();

        // The string is now valid UTF-8 again.
        debug_assert!(std::str::from_utf8(v).is_ok());
    }
}

#[inline]
pub(crate) fn encode_base64<T: AsRef<[u8]>>(data: T) -> String {
    use base64::Engine;
    fn encode_base64_impl(data: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(data)
    }
    encode_base64_impl(data.as_ref())
}

#[inline]
pub(crate) fn decode_base64<T: AsRef<[u8]>>(
    data: T,
) -> std::result::Result<Vec<u8>, base64::DecodeError> {
    use base64::Engine;
    fn decode_base64_impl(data: &[u8]) -> std::result::Result<Vec<u8>, base64::DecodeError> {
        base64::engine::general_purpose::STANDARD.decode(data)
    }
    decode_base64_impl(data.as_ref())
}

pub trait DisplaySliceExt<'s> {
    fn display_slice_tree<'a: 's>(&'a self, limit: usize) -> DisplayCellSlice<'a, 's>;

    fn display_slice_data<'a: 's>(&'a self) -> DisplaySliceData<'a, 's>;
}

impl<'s> DisplaySliceExt<'s> for CellSlice<'s> {
    fn display_slice_tree<'a: 's>(&'a self, limit: usize) -> DisplayCellSlice<'a, 's> {
        DisplayCellSlice { slice: self, limit }
    }

    fn display_slice_data<'a: 's>(&'a self) -> DisplaySliceData<'a, 's> {
        DisplaySliceData(self)
    }
}

pub struct DisplayCellSlice<'a, 'b> {
    slice: &'a CellSlice<'b>,
    limit: usize,
}

impl std::fmt::Display for DisplayCellSlice<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut stack = vec![(0, *self.slice)];

        let mut i = 0;
        while let Some((indent, cs)) = stack.pop() {
            i += 1;
            if i > self.limit {
                return f.write_str("<cell output limit reached>\n");
            }

            writeln!(f, "{:indent$}{}", "", DisplaySliceData(&cs))?;

            for cell in cs.references().rev() {
                // SAFETY: it is safe to print pruned branches
                let cs = unsafe { cell.as_slice_unchecked() };
                stack.push((indent + 1, cs));
            }
        }

        Ok(())
    }
}

pub struct DisplaySliceData<'a, 'b>(&'a CellSlice<'b>);

impl std::fmt::Display for DisplaySliceData<'_, '_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut cs = *self.0;

        if cs.cell_type().is_exotic() {
            f.write_str("SPECIAL ")?;
        }

        let mut buffer: [u8; 128] = [0; 128];

        let bits = cs.remaining_bits();
        cs.load_raw(&mut buffer, bits)
            .map_err(|_| std::fmt::Error)?;
        append_tag(&mut buffer, bits);

        let mut result = hex::encode(&buffer[..(bits as usize + 7) / 8]);
        if bits % 8 <= 4 {
            result.pop();
        }
        if bits % 4 != 0 {
            result.push('_');
        }

        write!(f, "x{{{}}}", result)
    }
}

fn append_tag(data: &mut [u8; 128], bit_len: u16) {
    debug_assert!(bit_len < 1024);

    let rem = bit_len % 8;
    let last_byte = (bit_len / 8) as usize;
    if rem > 0 {
        let last_byte = &mut data[last_byte];

        let tag_mask: u8 = 1 << (7 - rem);
        let data_mask = !(tag_mask - 1);

        *last_byte = (*last_byte & data_mask) | tag_mask;
    }
}

pub fn decode_hex_bitstring(s: &str) -> Result<CellBuilder> {
    fn hex_char(c: u8) -> Result<u8> {
        match c {
            b'A'..=b'F' => Ok(c - b'A' + 10),
            b'a'..=b'f' => Ok(c - b'a' + 10),
            b'0'..=b'9' => Ok(c - b'0'),
            _ => Err(Error::InvalidBitString),
        }
    }

    if !s.is_ascii() {
        return Err(Error::InvalidBitString);
    }

    let s = s.as_bytes();
    let (mut s, with_tag) = match s.strip_suffix(b"_") {
        Some(s) => (s, true),
        None => (s, false),
    };

    let mut half_byte = None;
    if s.len() % 2 != 0 {
        if let Some((last, prefix)) = s.split_last() {
            half_byte = Some(hex_char(*last)?);
            s = prefix;
        }
    }

    if s.len() > 128 * 2 {
        return Err(Error::InvalidBitString);
    }

    let mut builder = CellBuilder::new();

    let mut bytes = hex::decode(s).map_err(|_| Error::InvalidBitString)?;

    let mut bits = bytes.len() as u16 * 8;
    if let Some(half_byte) = half_byte {
        bits += 4;
        bytes.push(half_byte << 4);
    }

    if with_tag {
        bits = bytes.len() as u16 * 8;
        for byte in bytes.iter().rev() {
            if *byte == 0 {
                bits -= 8;
            } else {
                bits -= 1 + byte.trailing_zeros() as u16;
                break;
            }
        }
    }

    builder.store_raw(&bytes, bits)?;
    Ok(builder)
}

pub fn decode_binary_bitstring(s: &str) -> Result<CellBuilder> {
    let mut bits = 0;
    let mut buffer = [0; 128];

    for char in s.as_bytes() {
        let value = match char {
            b'0' => 0u8,
            b'1' => 1,
            _ => return Err(Error::InvalidBitString),
        };
        buffer[bits / 8] |= value << (7 - bits % 8);

        bits += 1;
        if bits > MAX_BIT_LEN as usize {
            return Err(Error::InvalidBitString);
        }
    }

    let mut builder = CellBuilder::new();
    builder.store_raw(&buffer, bits as u16)?;
    Ok(builder)
}
