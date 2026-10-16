use super::*;

#[test]
fn a_literal_is_its_type_then_its_value() {
    let mut out = Vec::new();
    literal(&mut out, &Literal::Int(300, Scalar::U16));
    assert_eq!(out, [0x00, 0x01, 0xAC, 0x02]);
    out.clear();
    literal(&mut out, &Literal::Int(-2, Scalar::I8));
    assert_eq!(out, [0x00, 0x04, 0x03]);
    out.clear();
    literal(&mut out, &Literal::Text("ok".to_owned()));
    assert_eq!(
        out,
        [
            0x03, 0x09, b'c', b'o', b'r', b'e', b':', b':', b'S', b't', b'r', 0x00, 0x02, b'o',
            b'k'
        ]
    );
}
