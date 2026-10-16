//! Hash values: the algorithm tag `0x01` and a BLAKE3-256 digest, with their checksummed base32
//! forms. The build script includes this file, so it depends on nothing but `core`.

/// A hash: the algorithm tag, then the 32-byte digest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hash(pub [u8; 33]);

const BLAKE3: u8 = 0x01;
const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

impl Hash {
    pub fn of(bytes: &[u8]) -> Hash {
        let mut hash = [BLAKE3; 33];
        hash[1..].copy_from_slice(&blake3(bytes));
        Hash(hash)
    }

    /// The checksummed base32 of all 33 bytes, 55 characters.
    pub fn full(&self) -> String {
        checksummed_base32(&self.0)
    }

    /// The checksummed base32 of the tag and the first 9 bytes of the digest, 18 characters.
    pub fn short(&self) -> String {
        checksummed_base32(&self.0[..10])
    }
}

/// The bytes a hash's short or full form writes: 18 or 55 characters of the alphabet with zero
/// padding bits and a correct checksum, over the tag `0x01`; none for any other text.
pub fn parse_form(text: &str) -> Option<Vec<u8>> {
    let length = match text.len() {
        18 => 10,
        55 => 33,
        _ => return None,
    };
    read_checksummed_base32(text, length).filter(|bytes| bytes[0] == BLAKE3)
}

/// The `length` bytes whose checksummed base32 is `text`, if any.
pub fn read_checksummed_base32(text: &str, length: usize) -> Option<Vec<u8>> {
    if text.len() != (length * 8).div_ceil(5) + 2 {
        return None;
    }
    let digits: Vec<u32> = text
        .bytes()
        .map(|byte| {
            ALPHABET
                .iter()
                .position(|letter| *letter == byte)
                .map(|at| at as u32)
        })
        .collect::<Option<_>>()?;
    let (data, _) = digits.split_at(digits.len() - 2);
    let mut bytes = Vec::with_capacity(length);
    let mut bits = 0u32;
    let mut held = 0;
    for digit in data {
        bits = (bits << 5) | digit;
        held += 5;
        if held >= 8 {
            held -= 8;
            bytes.push((bits >> held) as u8);
        }
    }
    let padding_zero = bits & ((1 << held) - 1) == 0;
    (bytes.len() == length && padding_zero && checksummed_base32(&bytes) == text).then_some(bytes)
}

/// The base32 of `bytes`, most significant bit first and the last character padded with zero
/// bits, followed by two characters encoding the first 10 bits of their BLAKE3-256 digest.
pub fn checksummed_base32(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut bits = 0u32;
    let mut held = 0;
    for byte in bytes {
        bits = (bits << 8) | u32::from(*byte);
        held += 8;
        while held >= 5 {
            held -= 5;
            out.push(char::from(ALPHABET[((bits >> held) & 31) as usize]));
        }
    }
    if held > 0 {
        out.push(char::from(ALPHABET[((bits << (5 - held)) & 31) as usize]));
    }
    let digest = blake3(bytes);
    let check = (u32::from(digest[0]) << 2) | (u32::from(digest[1]) >> 6);
    out.push(char::from(ALPHABET[(check >> 5) as usize]));
    out.push(char::from(ALPHABET[(check & 31) as usize]));
    out
}

const IV: [u32; 8] = [
    0x6A09_E667,
    0xBB67_AE85,
    0x3C6E_F372,
    0xA54F_F53A,
    0x510E_527F,
    0x9B05_688C,
    0x1F83_D9AB,
    0x5BE0_CD19,
];
const PERMUTATION: [usize; 16] = [2, 6, 3, 10, 7, 0, 4, 13, 1, 11, 12, 5, 9, 14, 15, 8];
const CHUNK_START: u32 = 1;
const CHUNK_END: u32 = 2;
const PARENT: u32 = 4;
const ROOT: u32 = 8;
const BLOCK: usize = 64;
const CHUNK: usize = 1024;

fn g(state: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize, x: u32, y: u32) {
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(x);
    state[d] = (state[d] ^ state[a]).rotate_right(16);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(12);
    state[a] = state[a].wrapping_add(state[b]).wrapping_add(y);
    state[d] = (state[d] ^ state[a]).rotate_right(8);
    state[c] = state[c].wrapping_add(state[d]);
    state[b] = (state[b] ^ state[c]).rotate_right(7);
}

fn compress(cv: &[u32; 8], block: &[u32; 16], counter: u64, len: u32, flags: u32) -> [u32; 16] {
    let mut state = [
        cv[0],
        cv[1],
        cv[2],
        cv[3],
        cv[4],
        cv[5],
        cv[6],
        cv[7],
        IV[0],
        IV[1],
        IV[2],
        IV[3],
        counter as u32,
        (counter >> 32) as u32,
        len,
        flags,
    ];
    let mut m = *block;
    for round in 0..7 {
        g(&mut state, 0, 4, 8, 12, m[0], m[1]);
        g(&mut state, 1, 5, 9, 13, m[2], m[3]);
        g(&mut state, 2, 6, 10, 14, m[4], m[5]);
        g(&mut state, 3, 7, 11, 15, m[6], m[7]);
        g(&mut state, 0, 5, 10, 15, m[8], m[9]);
        g(&mut state, 1, 6, 11, 12, m[10], m[11]);
        g(&mut state, 2, 7, 8, 13, m[12], m[13]);
        g(&mut state, 3, 4, 9, 14, m[14], m[15]);
        if round < 6 {
            m = PERMUTATION.map(|index| m[index]);
        }
    }
    for index in 0..8 {
        state[index] ^= state[index + 8];
        state[index + 8] ^= cv[index];
    }
    state
}

fn words(bytes: &[u8]) -> [u32; 16] {
    let mut block = [0u8; BLOCK];
    block[..bytes.len()].copy_from_slice(bytes);
    core::array::from_fn(|index| {
        u32::from_le_bytes([
            block[4 * index],
            block[4 * index + 1],
            block[4 * index + 2],
            block[4 * index + 3],
        ])
    })
}

fn first_eight(state: [u32; 16]) -> [u32; 8] {
    core::array::from_fn(|index| state[index])
}

/// The last compression of a chunk or parent, not yet known to be the root.
struct Output {
    cv: [u32; 8],
    block: [u32; 16],
    counter: u64,
    len: u32,
    flags: u32,
}

impl Output {
    fn chaining_value(&self) -> [u32; 8] {
        first_eight(compress(
            &self.cv,
            &self.block,
            self.counter,
            self.len,
            self.flags,
        ))
    }
}

fn parent(left: [u32; 8], right: [u32; 8]) -> Output {
    let mut block = [0; 16];
    block[..8].copy_from_slice(&left);
    block[8..].copy_from_slice(&right);
    Output {
        cv: IV,
        block,
        counter: 0,
        len: BLOCK as u32,
        flags: PARENT,
    }
}

/// The output of one chunk, from its 1 to 1024 bytes.
fn chunk(bytes: &[u8], counter: u64) -> Output {
    let mut cv = IV;
    let blocks = bytes.len().div_ceil(BLOCK).max(1);
    for index in 0..blocks - 1 {
        let start = if index == 0 { CHUNK_START } else { 0 };
        cv = first_eight(compress(
            &cv,
            &words(&bytes[index * BLOCK..(index + 1) * BLOCK]),
            counter,
            BLOCK as u32,
            start,
        ));
    }
    let last = &bytes[(blocks - 1) * BLOCK..];
    let start = if blocks == 1 { CHUNK_START } else { 0 };
    Output {
        cv,
        block: words(last),
        counter,
        len: last.len() as u32,
        flags: start | CHUNK_END,
    }
}

/// The BLAKE3-256 digest of `bytes`.
pub fn blake3(bytes: &[u8]) -> [u8; 32] {
    let chunks: Vec<&[u8]> = if bytes.is_empty() {
        vec![bytes]
    } else {
        bytes.chunks(CHUNK).collect()
    };
    let mut stack: Vec<[u32; 8]> = Vec::new();
    let last = chunks.len() - 1;
    for (index, data) in chunks[..last].iter().enumerate() {
        let mut cv = chunk(data, index as u64).chaining_value();
        let mut total = index as u64 + 1;
        while total & 1 == 0 {
            cv = parent(stack.pop().expect("a left subtree"), cv).chaining_value();
            total >>= 1;
        }
        stack.push(cv);
    }
    let mut output = chunk(chunks[last], last as u64);
    while let Some(left) = stack.pop() {
        output = parent(left, output.chaining_value());
    }
    let root = compress(
        &output.cv,
        &output.block,
        0,
        output.len,
        output.flags | ROOT,
    );
    let mut digest = [0; 32];
    for (index, word) in root[..8].iter().enumerate() {
        digest[4 * index..4 * index + 4].copy_from_slice(&word.to_le_bytes());
    }
    digest
}

#[cfg(test)]
mod tests;
