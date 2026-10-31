use super::Stub;
use crate::workspace::Arch;

/// An AArch64 system instruction, such as `TLBI`, `DC` or `IC`, of its fields and register.
fn sys(op1: u32, crn: u32, crm: u32, op2: u32, rt: u32) -> u32 {
    0xD508_0000 | op1 << 16 | crn << 12 | crm << 8 | op2 << 5 | rt
}

/// An AArch64 `MSR` or `MRS` of the system register its fields name.
fn system_register(read: bool, op0: u32, op1: u32, crn: u32, crm: u32, op2: u32, rt: u32) -> u32 {
    let base = if read { 0xD530_0000 } else { 0xD510_0000 };
    base | (op0 - 2) << 19 | op1 << 16 | crn << 12 | crm << 8 | op2 << 5 | rt
}

/// The AArch64 data synchronization barrier of a domain.
fn dsb(crm: u32) -> u32 {
    0xD503_309F | crm << 8
}

const ISB: u32 = 0xD503_3FDF;
const RET: u32 = 0xD65F_03C0;
const ISH: u32 = 0b1011;
const ISHST: u32 = 0b1010;
const SY: u32 = 0b1111;

/// The instructions of an AArch64 stub.
fn words(stub: Stub) -> Vec<u32> {
    let (_, _, bytes) = stub.code(Arch::Aarch64);
    assert_eq!(bytes.len() % 4, 0, "{stub:?} holds whole instructions");
    bytes
        .chunks(4)
        .map(|word| u32::from_le_bytes(word.try_into().expect("four bytes")))
        .collect()
}

/// Asserts that a stub's code holds `expected` in that order.
fn holds_in_order<T: PartialEq + std::fmt::Debug>(stub: Stub, code: &[T], expected: &[T]) {
    let mut rest = code;
    for item in expected {
        let Some(at) = rest.iter().position(|found| found == item) else {
            panic!("{stub:?} lacks {item:x?} in order in {code:x?}");
        };
        rest = &rest[at + 1..];
    }
}

#[test]
fn aarch64_translation_stubs_invalidate_what_they_name() {
    let tlbi = |crm, op2, rt| sys(0, 8, crm, op2, rt);
    let ttbr0 = |read| system_register(read, 3, 0, 2, 0, 0, if read { 10 } else { 0 });
    let cases = [
        (
            Stub::TlbiPage,
            vec![ttbr0(true), dsb(ISHST), tlbi(7, 1, 9), dsb(ISH), ISB],
        ),
        (
            Stub::TlbiAsid,
            vec![dsb(ISHST), tlbi(7, 2, 9), dsb(ISH), ISB],
        ),
        (
            Stub::TlbiAsidPage,
            vec![dsb(ISHST), tlbi(7, 1, 9), dsb(ISH), ISB],
        ),
        (
            Stub::TlbiAll,
            vec![dsb(ISHST), tlbi(7, 0, 31), dsb(ISH), ISB],
        ),
        (
            Stub::TlbiBroadcast,
            vec![dsb(ISHST), tlbi(3, 3, 9), tlbi(3, 0, 31), dsb(ISH), ISB],
        ),
        (Stub::WriteTtbr0Tagged, vec![dsb(ISHST), ttbr0(false), ISB]),
    ];
    for (stub, expected) in cases {
        let code = words(stub);
        holds_in_order(stub, &code, &expected);
        assert_eq!(code.last(), Some(&RET), "{stub:?} returns");
    }
}

#[test]
fn aarch64_cache_stubs_maintain_lines_of_the_cache_type_register() {
    let ctr = |rt| system_register(true, 3, 3, 0, 0, 1, rt);
    let dc = |op1, crm, rt| sys(op1, 7, crm, 1, rt);
    let cases = [
        (Stub::CleanCache, vec![ctr(9), dc(3, 10, 0), dsb(SY)]),
        (
            Stub::CleanInvalidateCache,
            vec![ctr(9), dc(3, 14, 0), dsb(SY)],
        ),
        (Stub::InvalidateCache, vec![ctr(9), dc(0, 6, 0), dsb(SY)]),
        (
            Stub::SyncInstructions,
            vec![
                ctr(12),
                dc(3, 11, 14),
                dsb(ISH),
                sys(3, 7, 5, 1, 14),
                dsb(ISH),
                ISB,
            ],
        ),
    ];
    for (stub, expected) in cases {
        let code = words(stub);
        holds_in_order(stub, &code, &expected);
        assert_eq!(code.last(), Some(&RET), "{stub:?} returns");
    }
}

#[test]
fn constant_time_sets_dit_and_doitm_where_enumerated() {
    let pfr0 = system_register(true, 3, 0, 0, 4, 0, 0);
    let dit = 0xD500_401F | 3 << 16 | 1 << 8 | 2 << 5;
    let code = words(Stub::ConstantTime);
    holds_in_order(Stub::ConstantTime, &code, &[pfr0, dit, RET]);
    let code = bytes(Stub::ConstantTime);
    let capabilities = [0xb9, 0x0a, 0x01, 0x00, 0x00, 0x0f, 0x32];
    let misc_control = [0xb9, 0x01, 0x1b, 0x00, 0x00, 0x0f, 0x32];
    let doitm = [0x83, 0xc8, 0x01, 0x0f, 0x30];
    for pattern in [&capabilities[..], &misc_control, &doitm] {
        assert!(contains(&code, pattern), "ConstantTime holds {pattern:x?}");
    }
}

#[test]
fn aarch64_signal_and_event_stubs() {
    let sgi1r = system_register(false, 3, 0, 12, 11, 5, 10);
    holds_in_order(
        Stub::SendSignal,
        &words(Stub::SendSignal),
        &[dsb(ISH), sgi1r, ISB, RET],
    );
    let ldaxr = 0xC85F_FC09;
    let wfe = 0xD503_205F;
    let clrex = 0xD503_3F5F;
    holds_in_order(
        Stub::WaitEvent,
        &words(Stub::WaitEvent),
        &[ldaxr, wfe, clrex, RET],
    );
}

/// The x86_64 code of a stub.
fn bytes(stub: Stub) -> Vec<u8> {
    stub.code(Arch::Amd64).2
}

/// The test of the running CPU's feature word against a mask of one byte.
fn feature_test(mask: u8) -> Vec<u8> {
    vec![0x65, 0xf6, 0x04, 0x25, 0xf8, 0x00, 0x00, 0x00, mask]
}

/// Whether `code` holds `pattern` as consecutive bytes.
fn contains(code: &[u8], pattern: &[u8]) -> bool {
    code.windows(pattern.len()).any(|window| window == pattern)
}

#[test]
fn x64_stubs_test_the_features_cpu_features_records() {
    let records = bytes(Stub::CpuFeatures);
    let sets = |mask: u32| match mask {
        0..0x80 => contains(&records, &[0x41, 0x83, 0xc8, mask as u8]),
        _ => contains(
            &records,
            &[&[0x41, 0x81, 0xc8][..], &mask.to_le_bytes()].concat(),
        ),
    };
    assert!(contains(
        &records,
        &[0x65, 0x4c, 0x89, 0x04, 0x25, 0xf8, 0x00, 0x00, 0x00]
    ));
    for (stub, mask) in [
        (Stub::Rdtscp, 0x01),
        (Stub::Invpcid, 0x02),
        (Stub::Rdrand, 0x04),
        (Stub::Rdseed, 0x08),
        (Stub::MonitorWait, 0x10),
    ] {
        assert!(
            contains(&bytes(stub), &feature_test(mask)),
            "{stub:?} tests {mask:#x}"
        );
        assert!(sets(u32::from(mask)), "CpuFeatures records {mask:#x}");
    }
    let protection = bytes(Stub::EnableProtection);
    let table = &protection[protection.len() - 32..];
    let entries: Vec<(u32, u32)> = table
        .chunks(8)
        .map(|entry| {
            let word = |at: usize| u32::from_le_bytes(entry[at..at + 4].try_into().expect("four"));
            (word(0), word(4))
        })
        .collect();
    let cr4 = [1 << 20, 1 << 21, 1 << 11, 1 << 17];
    for (index, (mask, bit)) in entries.into_iter().enumerate() {
        assert!(sets(mask), "CpuFeatures records {mask:#x}");
        assert_eq!(bit, cr4[index], "variant {index} sets its CR4 bit");
    }
}

#[test]
fn x64_stubs_run_their_instructions() {
    let cases: [(Stub, &[&[u8]]); 11] = [
        (Stub::Cpuid, &[&[0x53], &[0x0f, 0xa2], &[0x5b, 0xc3]]),
        (
            Stub::Rdtscp,
            &[&[0x0f, 0x01, 0xf9], &[0x0f, 0xae, 0xe8, 0x0f, 0x31]],
        ),
        (Stub::Rdrand, &[&[0x48, 0x0f, 0xc7, 0xf0]]),
        (Stub::Rdseed, &[&[0x48, 0x0f, 0xc7, 0xf8], &[0xf3, 0x90]]),
        (
            Stub::MonitorWait,
            &[
                &[0x0f, 0x01, 0xc8],
                &[0xb9, 0x01, 0x00, 0x00, 0x00, 0x0f, 0x01, 0xc9],
            ],
        ),
        (Stub::Invpcid, &[&[0x66, 0x0f, 0x38, 0x82, 0x3c, 0x24]]),
        (
            Stub::Shootdown,
            &[
                &[0x0f, 0x01, 0x3f],
                &[0x48, 0x81, 0xf1, 0x80, 0x00, 0x00, 0x00],
            ],
        ),
        (Stub::ReadXcr0, &[&[0x0f, 0x01, 0xd0]]),
        (Stub::WriteXcr0, &[&[0x0f, 0xa2], &[0x0f, 0x01, 0xd1]]),
        (Stub::ClearBuffers, &[&[0x6a, 0x10, 0x0f, 0x00, 0x2c, 0x24]]),
        (Stub::UserOpen, &[&[0x0f, 0x01, 0xcb]]),
    ];
    for (stub, expected) in cases {
        let code = bytes(stub);
        for pattern in expected {
            assert!(contains(&code, pattern), "{stub:?} holds {pattern:x?}");
        }
    }
    assert!(contains(&bytes(Stub::UserClose), &[0x0f, 0x01, 0xca]));
}
