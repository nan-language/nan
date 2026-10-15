//! Forms of the canonical NIR encoding. The build script includes this file, so it depends on
//! nothing but `std`.

/// An unsigned integer in LEB128, without redundant trailing groups.
pub fn uvar(out: &mut Vec<u8>, mut value: u128) {
    loop {
        let group = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(group);
            return;
        }
        out.push(group | 0x80);
    }
}

/// Bytes: their length as a uvar, then the bytes.
pub fn bytes(out: &mut Vec<u8>, value: &[u8]) {
    uvar(out, value.len() as u128);
    out.extend_from_slice(value);
}

#[cfg(test)]
mod tests;
