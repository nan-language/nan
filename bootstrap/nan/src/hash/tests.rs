use super::*;

fn hex(digest: [u8; 32]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Inputs of the official BLAKE3 test vectors: byte `i` is `i % 251`.
fn input(len: usize) -> Vec<u8> {
    (0..len).map(|index| (index % 251) as u8).collect()
}

#[test]
fn blake3_matches_the_reference_across_block_chunk_and_tree_boundaries() {
    let vectors = [
        (
            0,
            "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262",
        ),
        (
            1,
            "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        ),
        (
            64,
            "4eed7141ea4a5cd4b788606bd23f46e212af9cacebacdc7d1f4c6dc7f2511b98",
        ),
        (
            65,
            "de1e5fa0be70df6d2be8fffd0e99ceaa8eb6e8c93a63f2d8d1c30ecb6b263dee",
        ),
        (
            1024,
            "42214739f095a406f3fc83deb889744ac00df831c10daa55189b5d121c855af7",
        ),
        (
            1025,
            "d00278ae47eb27b34faecf67b4fe263f82d5412916c1ffd97c8cb7fb814b8444",
        ),
        (
            2049,
            "5f4d72f40d7a5f82b15ca2b2e44b1de3c2ef86c426c95c1af0b6879522563030",
        ),
        (
            3073,
            "7124b49501012f81cc7f11ca069ec9226cecb8a2c850cfe644e327d22d3e1cd3",
        ),
        (
            4097,
            "9b4052b38f1c5fc8b1f9ff7ac7b27cd242487b3d890d15c96a1c25b8aa0fb995",
        ),
        (
            8193,
            "bab6c09cb8ce8cf459261398d2e7aef35700bf488116ceb94a36d0f5f1b7bc3b",
        ),
        (
            31744,
            "62b6960e1a44bcc1eb1a611a8d6235b6b4b78f32e7abc4fb4c6cdcce94895c47",
        ),
        (
            102400,
            "bc3e3d41a1146b069abffad3c0d44860cf664390afce4d9661f7902e7943e085",
        ),
    ];
    for (len, digest) in vectors {
        assert_eq!(hex(blake3(&input(len))), digest, "length {len}");
    }
}

#[test]
fn checksummed_base32_pads_the_last_character_and_appends_ten_digest_bits() {
    assert_eq!(
        Hash::of(b"").full(),
        "agxrgsnz6x42djvaibg6unw4zfezxszfzgw4cevxzsnjhsxed4zgeb3"
    );
    assert_eq!(
        Hash::of(b"abc").full(),
        "afsdpm5mhbdfcm77wy5xkjz2rw2urrkyizoxtwyd7u2zy3gvxwoyker"
    );
    assert_eq!(
        checksummed_base32(&Hash::of(b"abc").0[..10]),
        "afsdpm5mhbdfcm7726"
    );
}

#[test]
fn hash_forms_read_back_and_reject_damage() {
    let hash = Hash::of(b"item\n");
    let (short, full) = (hash.short(), hash.full());
    assert_eq!((short.len(), full.len()), (18, 55));
    assert_eq!(parse_form(&short).as_deref(), Some(&hash.0[..10]));
    assert_eq!(parse_form(&full).as_deref(), Some(&hash.0[..]));
    let mut damaged = short.clone().into_bytes();
    damaged[5] = if damaged[5] == b'a' { b'b' } else { b'a' };
    assert_eq!(
        parse_form(std::str::from_utf8(&damaged).expect("ASCII")),
        None
    );
    assert_eq!(parse_form(&short[..17]), None);
    assert_eq!(parse_form(&checksummed_base32(&[2; 10])), None);
}
