use super::Class;

const EMIT: Class = Class::Irreversible(1);
const ERASE: Class = Class::Irreversible(4);

#[test]
fn irreversible_classes_join_and_meet_by_their_kinds() {
    assert_eq!(EMIT.join(ERASE), Class::Irreversible(5));
    assert_eq!(Class::Recorded.join(Class::Restorable), Class::Restorable);
    assert_eq!(EMIT.meet(ERASE), Class::Restorable);
    assert_eq!(Class::WORST.meet(ERASE), ERASE);
    assert!(EMIT.at_most(Class::Irreversible(5)) && !EMIT.at_most(ERASE));
    assert!(Class::Restorable.at_most(EMIT) && !EMIT.at_most(Class::Restorable));
}

#[test]
fn compensation_keeps_only_an_emission_irreversible() {
    assert_eq!(Class::WORST.compensated(), EMIT);
    assert_eq!(ERASE.compensated(), Class::Restorable);
    assert_eq!(Class::Recorded.compensated(), Class::Recorded);
}

#[test]
fn a_class_reads_back_from_its_text() {
    assert_eq!(
        Class::WORST.text(),
        "irreversible(emit, actuate, erase, spend)"
    );
    for class in [
        Class::Traceless,
        Class::Recorded,
        Class::Irreversible(5),
        Class::WORST,
    ] {
        assert_eq!(Class::parse(&class.text()), Some(class));
    }
}
