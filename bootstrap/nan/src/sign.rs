//! Ed25519 signatures (RFC 8032) over SHA-512, the text forms of keys and signatures, and the
//! message a seal signs. Verification accepts exactly the signatures whose public key and point R
//! are canonical encodings, whose scalar S is below the group order, whose public key does not
//! have small order, and for which `[S]B - [k]A` encodes as R.

use crate::hash::{Hash, checksummed_base32, read_checksummed_base32};

/// The text form of a public key: the checksummed base32 of its 32 bytes, 54 characters.
pub fn key_form(key: &[u8; 32]) -> String {
    checksummed_base32(key)
}

/// The public key a text form writes.
pub fn read_key(text: &str) -> Option<[u8; 32]> {
    read_checksummed_base32(text, 32)?.try_into().ok()
}

/// The text form of a signature: the checksummed base32 of its 64 bytes, 105 characters.
pub fn signature_form(signature: &[u8; 64]) -> String {
    checksummed_base32(signature)
}

/// The signature a text form writes.
pub fn read_signature(text: &str) -> Option<[u8; 64]> {
    read_checksummed_base32(text, 64)?.try_into().ok()
}

/// The bytes a principal signs to seal an item: `nan seal 1`, the principal, the item's canonical
/// path and the full forms of its sealed item hash and closure hash, each followed by LF.
pub fn seal_message(principal: &str, path: &str, sealed: Hash, closure: Hash) -> Vec<u8> {
    format!(
        "nan seal 1\n{principal}\n{path}\n{}\n{}\n",
        sealed.full(),
        closure.full()
    )
    .into_bytes()
}

/// A field element modulo 2^255 - 19: sixteen limbs of 16 bits, least significant first, each
/// possibly out of range between operations.
type Field = [i64; 16];

/// A point in extended coordinates (X, Y, Z, T), with x = X/Z, y = Y/Z and xy = T/Z.
type Point = [Field; 4];

const ZERO: Field = [0; 16];
const ONE: Field = [1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
/// The curve constant d = -121665/121666.
const D: Field = [
    0x78a3, 0x1359, 0x4dca, 0x75eb, 0xd8ab, 0x4141, 0x0a4d, 0x0070, 0xe898, 0x7779, 0x4079, 0x8cc7,
    0xfe73, 0x2b6f, 0x6cee, 0x5203,
];
const D2: Field = [
    0xf159, 0x26b2, 0x9b94, 0xebd6, 0xb156, 0x8283, 0x149a, 0x00e0, 0xd130, 0xeef3, 0x80f2, 0x198e,
    0xfce7, 0x56df, 0xd9dc, 0x2406,
];
/// The coordinates of the base point B.
const BASE_X: Field = [
    0xd51a, 0x8f25, 0x2d60, 0xc956, 0xa7b2, 0x9525, 0xc760, 0x692c, 0xdc5c, 0xfdd6, 0xe231, 0xc0a4,
    0x53fe, 0xcd6e, 0x36d3, 0x2169,
];
const BASE_Y: Field = [
    0x6658, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666, 0x6666,
    0x6666, 0x6666, 0x6666, 0x6666,
];
/// A square root of -1.
const SQRT_M1: Field = [
    0xa0b0, 0x4a0e, 0x1b27, 0xc4ee, 0xe478, 0xad2f, 0x1806, 0x2f43, 0xd7a7, 0x3dfb, 0x0099, 0x2b4d,
    0xdf0b, 0x4fc1, 0x2480, 0x2b83,
];
/// The group order L, least significant byte first.
const ORDER: [i64; 32] = [
    0xed, 0xd3, 0xf5, 0x5c, 0x1a, 0x63, 0x12, 0x58, 0xd6, 0x9c, 0xf7, 0xa2, 0xde, 0xf9, 0xde, 0x14,
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x10,
];

fn carry(o: &mut Field) {
    for i in 0..16 {
        o[i] += 1 << 16;
        let c = o[i] >> 16;
        match i {
            15 => o[0] += 38 * (c - 1),
            _ => o[i + 1] += c - 1,
        }
        o[i] -= c << 16;
    }
}

/// Swaps `p` and `q` when `swap` is 1, in time independent of it.
fn select(p: &mut Field, q: &mut Field, swap: i64) {
    let mask = !(swap - 1);
    for i in 0..16 {
        let t = mask & (p[i] ^ q[i]);
        p[i] ^= t;
        q[i] ^= t;
    }
}

/// The canonical encoding of a field element: its value below 2^255 - 19, little-endian.
fn pack(n: &Field) -> [u8; 32] {
    let mut t = *n;
    carry(&mut t);
    carry(&mut t);
    carry(&mut t);
    for _ in 0..2 {
        let mut m = ZERO;
        m[0] = t[0] - 0xffed;
        for i in 1..15 {
            m[i] = t[i] - 0xffff - ((m[i - 1] >> 16) & 1);
            m[i - 1] &= 0xffff;
        }
        m[15] = t[15] - 0x7fff - ((m[14] >> 16) & 1);
        let borrow = (m[15] >> 16) & 1;
        m[14] &= 0xffff;
        select(&mut t, &mut m, 1 - borrow);
    }
    let mut out = [0; 32];
    for i in 0..16 {
        out[2 * i] = (t[i] & 0xff) as u8;
        out[2 * i + 1] = (t[i] >> 8) as u8;
    }
    out
}

/// The field element of the low 255 bits of an encoding.
fn unpack(bytes: &[u8; 32]) -> Field {
    let mut o = ZERO;
    for i in 0..16 {
        o[i] = i64::from(bytes[2 * i]) + (i64::from(bytes[2 * i + 1]) << 8);
    }
    o[15] &= 0x7fff;
    o
}

fn add(a: &Field, b: &Field) -> Field {
    core::array::from_fn(|i| a[i] + b[i])
}

fn sub(a: &Field, b: &Field) -> Field {
    core::array::from_fn(|i| a[i] - b[i])
}

fn mul(a: &Field, b: &Field) -> Field {
    let mut t = [0i64; 31];
    for i in 0..16 {
        for j in 0..16 {
            t[i + j] += a[i] * b[j];
        }
    }
    for i in 0..15 {
        t[i] += 38 * t[i + 16];
    }
    let mut o: Field = core::array::from_fn(|i| t[i]);
    carry(&mut o);
    carry(&mut o);
    o
}

fn square(a: &Field) -> Field {
    mul(a, a)
}

/// a^(p - 2), the inverse of a nonzero a.
fn invert(a: &Field) -> Field {
    let mut c = *a;
    for bit in (0..=253).rev() {
        c = square(&c);
        if bit != 2 && bit != 4 {
            c = mul(&c, a);
        }
    }
    c
}

/// a^((p - 5) / 8).
fn pow_p58(a: &Field) -> Field {
    let mut c = *a;
    for bit in (0..=250).rev() {
        c = square(&c);
        if bit != 1 {
            c = mul(&c, a);
        }
    }
    c
}

fn equal(a: &Field, b: &Field) -> bool {
    pack(a) == pack(b)
}

/// The low bit of a field element's canonical value.
fn parity(a: &Field) -> u8 {
    pack(a)[0] & 1
}

fn neutral() -> Point {
    [ZERO, ONE, ONE, ZERO]
}

fn base() -> Point {
    [BASE_X, BASE_Y, ONE, mul(&BASE_X, &BASE_Y)]
}

fn point_add(p: &Point, q: &Point) -> Point {
    let a = mul(&sub(&p[1], &p[0]), &sub(&q[1], &q[0]));
    let b = mul(&add(&p[0], &p[1]), &add(&q[0], &q[1]));
    let c = mul(&mul(&p[3], &q[3]), &D2);
    let d = mul(&p[2], &q[2]);
    let d = add(&d, &d);
    let (e, f, g, h) = (sub(&b, &a), sub(&d, &c), add(&d, &c), add(&b, &a));
    [mul(&e, &f), mul(&h, &g), mul(&g, &f), mul(&e, &h)]
}

fn point_select(p: &mut Point, q: &mut Point, swap: i64) {
    for (left, right) in p.iter_mut().zip(q.iter_mut()) {
        select(left, right, swap);
    }
}

fn encode(p: &Point) -> [u8; 32] {
    let inverse = invert(&p[2]);
    let x = mul(&p[0], &inverse);
    let y = mul(&p[1], &inverse);
    let mut out = pack(&y);
    out[31] ^= parity(&x) << 7;
    out
}

/// [s]q for a little-endian scalar s, by a ladder in time independent of s.
fn scalar_mul(q: &Point, s: &[u8; 32]) -> Point {
    let mut p = neutral();
    let mut q = *q;
    for bit in (0..256).rev() {
        let swap = i64::from((s[bit / 8] >> (bit & 7)) & 1);
        point_select(&mut p, &mut q, swap);
        q = point_add(&q, &p);
        p = point_add(&p, &p);
        point_select(&mut p, &mut q, swap);
    }
    p
}

/// The point an encoding names: none when its y is not below 2^255 - 19, no x satisfies the
/// curve equation, or x is 0 with the sign bit set.
fn decode(bytes: &[u8; 32]) -> Option<Point> {
    let y = unpack(bytes);
    let mut low = *bytes;
    low[31] &= 0x7f;
    if pack(&y) != low {
        return None;
    }
    let y2 = square(&y);
    let u = sub(&y2, &ONE);
    let v = add(&mul(&y2, &D), &ONE);
    let v3 = mul(&square(&v), &v);
    let mut x = mul(&mul(&pow_p58(&mul(&mul(&square(&v3), &v), &u)), &v3), &u);
    let check = mul(&square(&x), &v);
    if !equal(&check, &u) {
        if !equal(&check, &sub(&ZERO, &u)) {
            return None;
        }
        x = mul(&x, &SQRT_M1);
    }
    let sign = bytes[31] >> 7;
    if equal(&x, &ZERO) && sign == 1 {
        return None;
    }
    if parity(&x) != sign {
        x = sub(&ZERO, &x);
    }
    Some([x, y, ONE, mul(&x, &y)])
}

/// The value of 64 little-endian limbs modulo L, as 32 bytes.
fn reduce_limbs(x: &mut [i64; 64]) -> [u8; 32] {
    for i in (32..64).rev() {
        let mut c = 0;
        for j in (i - 32)..(i - 12) {
            x[j] += c - 16 * x[i] * ORDER[j - (i - 32)];
            c = (x[j] + 128) >> 8;
            x[j] -= c << 8;
        }
        x[i - 12] += c;
        x[i] = 0;
    }
    let mut c = 0;
    for j in 0..32 {
        x[j] += c - (x[31] >> 4) * ORDER[j];
        c = x[j] >> 8;
        x[j] &= 255;
    }
    for j in 0..32 {
        x[j] -= c * ORDER[j];
    }
    let mut out = [0; 32];
    for i in 0..32 {
        x[i + 1] += x[i] >> 8;
        out[i] = (x[i] & 255) as u8;
    }
    out
}

/// A 64-byte little-endian number modulo L.
fn reduce(bytes: &[u8; 64]) -> [u8; 32] {
    let mut x: [i64; 64] = core::array::from_fn(|i| i64::from(bytes[i]));
    reduce_limbs(&mut x)
}

/// Whether a 32-byte little-endian number is below L.
fn below_order(s: &[u8; 32]) -> bool {
    for i in (0..32).rev() {
        let limb = ORDER[i] as u8;
        if s[i] != limb {
            return s[i] < limb;
        }
    }
    false
}

/// The secret scalar and the prefix of a seed.
fn expanded(seed: &[u8; 32]) -> ([u8; 32], [u8; 32]) {
    let digest = sha512(seed);
    let mut scalar: [u8; 32] = digest[..32].try_into().expect("32 bytes");
    scalar[0] &= 248;
    scalar[31] &= 127;
    scalar[31] |= 64;
    (scalar, digest[32..].try_into().expect("32 bytes"))
}

/// The scalar of the digest of `parts` modulo L.
fn hashed(parts: &[&[u8]]) -> [u8; 32] {
    reduce(&sha512(&parts.concat()))
}

/// The public key of a 32-byte secret seed.
pub fn public_key(seed: &[u8; 32]) -> [u8; 32] {
    encode(&scalar_mul(&base(), &expanded(seed).0))
}

/// The signature of a message by the key of a seed: R and S.
pub fn sign(seed: &[u8; 32], message: &[u8]) -> [u8; 64] {
    let (scalar, prefix) = expanded(seed);
    let key = encode(&scalar_mul(&base(), &scalar));
    let r = hashed(&[&prefix, message]);
    let point = encode(&scalar_mul(&base(), &r));
    let k = hashed(&[&point, &key, message]);
    let mut x = [0i64; 64];
    for i in 0..32 {
        x[i] = i64::from(r[i]);
    }
    for i in 0..32 {
        for j in 0..32 {
            x[i + j] += i64::from(k[i]) * i64::from(scalar[j]);
        }
    }
    let s = reduce_limbs(&mut x);
    let mut signature = [0; 64];
    signature[..32].copy_from_slice(&point);
    signature[32..].copy_from_slice(&s);
    signature
}

/// Whether `signature` is a signature of `message` by `key`.
pub fn verify(key: &[u8; 32], message: &[u8], signature: &[u8; 64]) -> bool {
    let Some(point) = decode(key) else {
        return false;
    };
    let s: [u8; 32] = signature[32..].try_into().expect("32 bytes");
    if !below_order(&s) {
        return false;
    }
    let mut eight = point;
    for _ in 0..3 {
        eight = point_add(&eight, &eight);
    }
    if encode(&eight) == encode(&neutral()) {
        return false;
    }
    let negated = [
        sub(&ZERO, &point[0]),
        point[1],
        point[2],
        sub(&ZERO, &point[3]),
    ];
    let k = hashed(&[&signature[..32], key, message]);
    let check = point_add(&scalar_mul(&negated, &k), &scalar_mul(&base(), &s));
    encode(&check)[..] == signature[..32]
}

const ROUND: [u64; 80] = [
    0x428A_2F98_D728_AE22,
    0x7137_4491_23EF_65CD,
    0xB5C0_FBCF_EC4D_3B2F,
    0xE9B5_DBA5_8189_DBBC,
    0x3956_C25B_F348_B538,
    0x59F1_11F1_B605_D019,
    0x923F_82A4_AF19_4F9B,
    0xAB1C_5ED5_DA6D_8118,
    0xD807_AA98_A303_0242,
    0x1283_5B01_4570_6FBE,
    0x2431_85BE_4EE4_B28C,
    0x550C_7DC3_D5FF_B4E2,
    0x72BE_5D74_F27B_896F,
    0x80DE_B1FE_3B16_96B1,
    0x9BDC_06A7_25C7_1235,
    0xC19B_F174_CF69_2694,
    0xE49B_69C1_9EF1_4AD2,
    0xEFBE_4786_384F_25E3,
    0x0FC1_9DC6_8B8C_D5B5,
    0x240C_A1CC_77AC_9C65,
    0x2DE9_2C6F_592B_0275,
    0x4A74_84AA_6EA6_E483,
    0x5CB0_A9DC_BD41_FBD4,
    0x76F9_88DA_8311_53B5,
    0x983E_5152_EE66_DFAB,
    0xA831_C66D_2DB4_3210,
    0xB003_27C8_98FB_213F,
    0xBF59_7FC7_BEEF_0EE4,
    0xC6E0_0BF3_3DA8_8FC2,
    0xD5A7_9147_930A_A725,
    0x06CA_6351_E003_826F,
    0x1429_2967_0A0E_6E70,
    0x27B7_0A85_46D2_2FFC,
    0x2E1B_2138_5C26_C926,
    0x4D2C_6DFC_5AC4_2AED,
    0x5338_0D13_9D95_B3DF,
    0x650A_7354_8BAF_63DE,
    0x766A_0ABB_3C77_B2A8,
    0x81C2_C92E_47ED_AEE6,
    0x9272_2C85_1482_353B,
    0xA2BF_E8A1_4CF1_0364,
    0xA81A_664B_BC42_3001,
    0xC24B_8B70_D0F8_9791,
    0xC76C_51A3_0654_BE30,
    0xD192_E819_D6EF_5218,
    0xD699_0624_5565_A910,
    0xF40E_3585_5771_202A,
    0x106A_A070_32BB_D1B8,
    0x19A4_C116_B8D2_D0C8,
    0x1E37_6C08_5141_AB53,
    0x2748_774C_DF8E_EB99,
    0x34B0_BCB5_E19B_48A8,
    0x391C_0CB3_C5C9_5A63,
    0x4ED8_AA4A_E341_8ACB,
    0x5B9C_CA4F_7763_E373,
    0x682E_6FF3_D6B2_B8A3,
    0x748F_82EE_5DEF_B2FC,
    0x78A5_636F_4317_2F60,
    0x84C8_7814_A1F0_AB72,
    0x8CC7_0208_1A64_39EC,
    0x90BE_FFFA_2363_1E28,
    0xA450_6CEB_DE82_BDE9,
    0xBEF9_A3F7_B2C6_7915,
    0xC671_78F2_E372_532B,
    0xCA27_3ECE_EA26_619C,
    0xD186_B8C7_21C0_C207,
    0xEADA_7DD6_CDE0_EB1E,
    0xF57D_4F7F_EE6E_D178,
    0x06F0_67AA_7217_6FBA,
    0x0A63_7DC5_A2C8_98A6,
    0x113F_9804_BEF9_0DAE,
    0x1B71_0B35_131C_471B,
    0x28DB_77F5_2304_7D84,
    0x32CA_AB7B_40C7_2493,
    0x3C9E_BE0A_15C9_BEBC,
    0x431D_67C4_9C10_0D4C,
    0x4CC5_D4BE_CB3E_42B6,
    0x597F_299C_FC65_7E2A,
    0x5FCB_6FAB_3AD6_FAEC,
    0x6C44_198C_4A47_5817,
];

const INITIAL: [u64; 8] = [
    0x6A09_E667_F3BC_C908,
    0xBB67_AE85_84CA_A73B,
    0x3C6E_F372_FE94_F82B,
    0xA54F_F53A_5F1D_36F1,
    0x510E_527F_ADE6_82D1,
    0x9B05_688C_2B3E_6C1F,
    0x1F83_D9AB_FB41_BD6B,
    0x5BE0_CD19_137E_2179,
];

fn compress(state: &mut [u64; 8], block: &[u8; 128]) {
    let mut w = [0u64; 80];
    for (i, word) in block.as_chunks::<8>().0.iter().enumerate() {
        w[i] = u64::from_be_bytes(*word);
    }
    for i in 16..80 {
        let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
        let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let mut v = *state;
    for i in 0..80 {
        let s1 = v[4].rotate_right(14) ^ v[4].rotate_right(18) ^ v[4].rotate_right(41);
        let choice = (v[4] & v[5]) ^ (!v[4] & v[6]);
        let t1 = v[7]
            .wrapping_add(s1)
            .wrapping_add(choice)
            .wrapping_add(ROUND[i])
            .wrapping_add(w[i]);
        let s0 = v[0].rotate_right(28) ^ v[0].rotate_right(34) ^ v[0].rotate_right(39);
        let majority = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        let t2 = s0.wrapping_add(majority);
        v = [
            t1.wrapping_add(t2),
            v[0],
            v[1],
            v[2],
            v[3].wrapping_add(t1),
            v[4],
            v[5],
            v[6],
        ];
    }
    for i in 0..8 {
        state[i] = state[i].wrapping_add(v[i]);
    }
}

/// The SHA-512 digest of `bytes`.
pub fn sha512(bytes: &[u8]) -> [u8; 64] {
    let mut state = INITIAL;
    let mut padded = bytes.to_vec();
    padded.push(0x80);
    while padded.len() % 128 != 112 {
        padded.push(0);
    }
    padded.extend((bytes.len() as u128 * 8).to_be_bytes());
    for block in padded.as_chunks::<128>().0 {
        compress(&mut state, block);
    }
    let mut digest = [0; 64];
    for (i, word) in state.iter().enumerate() {
        digest[8 * i..8 * i + 8].copy_from_slice(&word.to_be_bytes());
    }
    digest
}

#[cfg(test)]
mod tests;
