use super::*;

#[test]
fn uvar_writes_seven_bits_per_byte_low_group_first() {
    let mut out = Vec::new();
    uvar(&mut out, 300);
    assert_eq!(out, [0xAC, 0x02]);
    out.clear();
    uvar(&mut out, 0);
    assert_eq!(out, [0x00]);
}
