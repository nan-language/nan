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

/// A signed integer: the uvar 2n when n is at least 0, and −2n − 1 otherwise.
pub fn svar(out: &mut Vec<u8>, value: i128) {
    let folded = match value >= 0 {
        true => (value as u128) << 1,
        false => ((-(value + 1)) as u128) << 1 | 1,
    };
    uvar(out, folded);
}

/// A list: its count, then each element as `each` writes it.
pub fn list<T>(
    out: &mut Vec<u8>,
    items: impl ExactSizeIterator<Item = T>,
    mut each: impl FnMut(&mut Vec<u8>, T),
) {
    uvar(out, items.len() as u128);
    for item in items {
        each(out, item);
    }
}

/// A record being written: its fields, each a key and its content, keys strictly increasing; an
/// absent field is never added.
#[derive(Clone, Default)]
pub struct Record {
    fields: Vec<(u64, Vec<u8>)>,
}

impl Record {
    pub fn field(&mut self, key: u64, content: Vec<u8>) {
        debug_assert!(self.fields.last().is_none_or(|(last, _)| *last < key));
        self.fields.push((key, content));
    }

    /// The record: its field count, then each field's key as a uvar and its content as bytes.
    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        list(&mut out, self.fields.into_iter(), |out, (key, content)| {
            uvar(out, u128::from(key));
            bytes(out, &content);
        });
        out
    }
}

#[cfg(test)]
mod tests;
