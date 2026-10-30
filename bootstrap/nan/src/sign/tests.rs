use super::*;

fn decoded(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&hex[at..at + 2], 16).expect("hex digits"))
        .collect()
}

fn bytes<const N: usize>(hex: &str) -> [u8; N] {
    decoded(hex).try_into().expect("the length")
}

fn hex(held: &[u8]) -> String {
    held.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The test vectors 1 to 3 of RFC 8032 section 7.1: seed, public key, message and signature.
const VECTORS: [(&str, &str, &str, &str); 3] = [
    (
        "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        "",
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
    ),
    (
        "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
        "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        "72",
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
    ),
    (
        "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
        "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        "af82",
        "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
    ),
];

#[test]
fn sha512_matches_the_reference_within_and_across_blocks() {
    assert_eq!(
        hex(&sha512(b"abc")),
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    let long: Vec<u8> = (0..300).map(|index| (index % 251) as u8).collect();
    assert_eq!(
        hex(&sha512(&long)),
        "f758e49e6fca32320310aa1b0b2e3abfeab7fc8414ba069022e8b39b7e53628be4777edbab93a6780a8318eed48c4665ff55d68dff2e6c84b6270b4eff6df873"
    );
}

#[test]
fn keys_and_signatures_match_rfc_8032_and_verify() {
    for (seed, key, message, signature) in VECTORS {
        let (seed, key, message) = (bytes::<32>(seed), bytes::<32>(key), decoded(message));
        assert_eq!(public_key(&seed), key);
        let signed = sign(&seed, &message);
        assert_eq!(hex(&signed), signature);
        assert!(verify(&key, &message, &signed));
    }
}

#[test]
fn verification_rejects_tampering_malleability_and_weak_keys() {
    let (_, key, _, signature) = VECTORS[0];
    let (key, signature) = (bytes::<32>(key), bytes::<64>(signature));
    assert!(!verify(&key, b"x", &signature));
    let mut flipped = signature;
    flipped[0] ^= 1;
    assert!(!verify(&key, b"", &flipped));
    let mut malleable = signature;
    malleable[32..].copy_from_slice(&bytes::<32>(
        "4c8c7872aa064e049dbb3013fbf29380d25bf5f0595bbe24655141438e7a101b",
    ));
    assert!(!verify(&key, b"", &malleable));
    let neutral = bytes::<32>("0100000000000000000000000000000000000000000000000000000000000000");
    assert!(!verify(&neutral, b"", &signature));
    let negative_zero =
        bytes::<32>("0100000000000000000000000000000000000000000000000000000000000080");
    assert!(decode(&negative_zero).is_none());
    let canonical = bytes::<32>("0500000000000000000000000000000000000000000000000000000000000000");
    let unreduced = bytes::<32>("f2ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff7f");
    assert!(decode(&canonical).is_some());
    assert!(decode(&unreduced).is_none());
}
