use super::aarch64::{self, VBAR_SEAM};
use super::interrupts::{SEAM_COMMON, SEAM_DISPATCH, SEAM_PREEMPT, SEAM_STAY};
use super::stubs::Stub;
use crate::workspace::Arch;

/// The AArch64 instructions of some machine code.
fn words(code: &[u8]) -> Vec<u32> {
    assert_eq!(code.len() % 4, 0, "whole instructions");
    code.chunks(4)
        .map(|word| u32::from_le_bytes(word.try_into().expect("four bytes")))
        .collect()
}

/// Whether an AArch64 instruction uses the floating-point and Advanced SIMD registers, which
/// trap with `CPACR_EL1.FPEN` clear: a scalar floating-point or SIMD operation, a load or store
/// of those registers, or an access to `FPCR` or `FPSR`.
fn touches_floats(word: u32) -> bool {
    let group = word >> 25 & 0xF;
    let simd = group & 0b0111 == 0b0111;
    let vector_memory = group & 0b0101 == 0b0100 && word >> 26 & 1 == 1;
    let control = word & 0xFFDF_FFC0 == 0xD51B_4400;
    simd || vector_memory || control
}

/// Whether x86-64 code holds an `fxsave`, `fxrstor`, `xsave` or `xrstor` of memory.
fn saves_extended_state(code: &[u8]) -> bool {
    code.windows(3).any(|bytes| {
        let (modrm_mode, reg) = (bytes[2] >> 6, bytes[2] >> 3 & 7);
        bytes[..2] == [0x0f, 0xae] && modrm_mode != 3 && matches!(reg, 0 | 1 | 4 | 5)
    })
}

#[test]
fn x64_seam_saves_registers_alone_and_calls_where_relocated() {
    assert!(!saves_extended_state(SEAM_COMMON));
    let stay = SEAM_STAY as usize;
    assert_eq!(SEAM_COMMON[..2], [0x41, 0x57], "the first entry pushes r15");
    assert_eq!(
        SEAM_COMMON[stay..stay + 2],
        [0x41, 0x57],
        "the second entry pushes r15"
    );
    for call in [SEAM_DISPATCH, SEAM_PREEMPT] {
        let at = call as usize;
        assert_eq!(SEAM_COMMON[at - 1], 0xe8, "a call precedes {call:#x}");
        assert_eq!(
            SEAM_COMMON[at..at + 4],
            [0; 4],
            "the displacement at {call:#x}"
        );
    }
}

#[test]
fn aarch64_seam_frames_hold_the_general_state_alone() {
    let code = words(VBAR_SEAM);
    let bytes = aarch64::frame::FAR as u32 + 8;
    let sub_sp = 0xD100_03FF | bytes << 10;
    let entries = code.iter().filter(|word| **word == sub_sp).count();
    assert_eq!(
        entries, 12,
        "each entry but those for AArch32 opens a frame of {bytes} bytes"
    );
    let pop = 0x9100_03FF | bytes << 10;
    assert!(code.contains(&pop), "the return pops the frame");
    let pairs = 0xD280_000C | (bytes / 16) << 5;
    assert!(
        code.contains(&pairs),
        "preemption copies the frame by pairs"
    );
    for word in &code {
        assert!(
            !touches_floats(*word),
            "{word:#010x} touches the floating-point registers"
        );
    }
    for call in [aarch64::SEAM_DISPATCH, aarch64::SEAM_PREEMPT] {
        assert_eq!(
            code[call as usize / 4] >> 26,
            0b10_0101,
            "a bl at {call:#x}"
        );
    }
}

#[test]
fn aarch64_machine_stubs_use_no_floating_point_register() {
    let machine = [
        Stub::MmioRead1,
        Stub::MmioRead2,
        Stub::MmioRead4,
        Stub::MmioRead8,
        Stub::MmioWrite1,
        Stub::MmioWrite2,
        Stub::MmioWrite4,
        Stub::MmioWrite8,
        Stub::Fill,
        Stub::Copy,
        Stub::Pause,
        Stub::Mfence,
        Stub::IdleWait,
        Stub::Sti,
        Stub::Cli,
        Stub::IrqSave,
        Stub::IrqRestore,
        Stub::Thread,
        Stub::SetCpu,
        Stub::Halt,
        Stub::DisableFloats,
        Stub::ConstantTime,
        Stub::ReadTtbr0,
        Stub::WriteTtbr0,
        Stub::WriteTtbr0Tagged,
        Stub::TlbiPage,
        Stub::TlbiAsid,
        Stub::TlbiAsidPage,
        Stub::TlbiAll,
        Stub::TlbiBroadcast,
        Stub::CleanCache,
        Stub::CleanInvalidateCache,
        Stub::InvalidateCache,
        Stub::SyncInstructions,
        Stub::WaitEvent,
        Stub::SendSignal,
        Stub::EnterRoot,
        Stub::InterruptStack,
        Stub::MachineSwitch,
        Stub::SemiExit,
        Stub::Hvc,
        Stub::FiqOn,
        Stub::FiqOff,
    ];
    for stub in machine {
        let (_, _, code) = stub.code(Arch::Aarch64);
        for word in words(&code) {
            assert!(!touches_floats(word), "{stub:?} holds {word:#010x}");
        }
    }
    let (_, _, host) = Stub::Switch.code(Arch::Aarch64);
    let saves_d8 = 0x6D00_27E8;
    assert!(
        words(&host)
            .iter()
            .any(|word| word & 0xFFC0_7FFF == saves_d8),
        "the host switch keeps d8 to d15"
    );
}

#[test]
fn disable_floats_sets_task_switched_and_clears_fpen() {
    let (_, _, x64) = Stub::DisableFloats.code(Arch::Amd64);
    assert_eq!(
        x64,
        [
            0x0f, 0x20, 0xc0, 0x48, 0x83, 0xc8, 0x08, 0x0f, 0x22, 0xc0, 0xc3
        ]
    );
    let (_, _, a64) = Stub::DisableFloats.code(Arch::Aarch64);
    let cpacr = |read: bool| if read { 0xD538_1040 } else { 0xD518_1040 };
    let clear_fpen = 0x926A_F400;
    let isb = 0xD503_3FDF;
    let ret = 0xD65F_03C0;
    assert_eq!(
        words(&a64),
        [cpacr(true), clear_fpen, cpacr(false), isb, ret]
    );
}
