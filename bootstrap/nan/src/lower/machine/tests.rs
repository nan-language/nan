use super::aarch64::{self, VBAR_SEAM};
use super::handoff::PVH_TRAMPOLINE;
use super::interrupts::{RESERVED, SEAM_COMMON, SEAM_DISPATCH, SEAM_PREEMPT, SEAM_STAY};
use super::stubs::Stub;
use super::{AARCH64_BARE_SEAM, AARCH64_SEAM_BOOT, AARCH64_SEAM_ROOT, AARCH64_SEAM_STACK};
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
    let mut below = vec![0x48, 0x81, 0xef];
    below.extend_from_slice(&(176 + RESERVED as u32).to_le_bytes());
    assert!(
        SEAM_COMMON.windows(below.len()).any(|bytes| bytes == below),
        "preemption copies the frame {RESERVED} bytes below the interrupted stack pointer"
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
    let copy = 0xD100_0129 | (bytes + RESERVED as u32) << 10;
    assert!(
        code.contains(&copy),
        "preemption copies the frame {RESERVED} bytes below the interrupted stack pointer"
    );
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
        Stub::EnterImage,
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

/// The byte offset an AArch64 `ldr` of a 64-bit literal or a `cbnz` at `at` reaches.
fn a64_target(word: u32, at: usize) -> usize {
    let offset = (word >> 5 & 0x7_FFFF) as usize * 4;
    at + offset
}

#[test]
fn aarch64_bare_seam_passes_x0_and_x1_on_and_finds_its_pool() {
    let code = words(AARCH64_BARE_SEAM);
    let pool: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, word)| *word & 0xFF00_001F == 0x5800_0009)
        .map(|(index, word)| a64_target(*word, index * 4))
        .collect();
    let patched = [AARCH64_SEAM_ROOT, AARCH64_SEAM_STACK, AARCH64_SEAM_BOOT];
    for offset in patched {
        assert_eq!(offset % 8, 0, "the literal at {offset:#x} is aligned");
        assert!(
            pool.contains(&(offset as usize)),
            "an ldr reads {offset:#x}"
        );
    }
    let call = code
        .iter()
        .position(|word| *word == 0xD63F_0120)
        .expect("a blr x9");
    let writes = |register: u32| {
        code[..call]
            .iter()
            .filter(|word| *word & 0x1F == register && *word >> 24 != 0xB5 && *word >> 24 != 0xD5)
            .count()
    };
    assert_eq!(writes(1), 0, "x1 reaches the call as entered");
    assert_eq!(
        writes(0),
        1,
        "only the default device tree's address replaces x0"
    );
    let set = code
        .iter()
        .position(|word| *word == 0xD2A8_0000)
        .expect("a mov x0");
    for register in [0, 1] {
        let skip = code
            .iter()
            .position(|word| *word >> 24 == 0xB5 && *word & 0x1F == register)
            .expect("a cbnz over the default");
        assert_eq!(a64_target(code[skip], skip * 4), (set + 1) * 4);
    }
}

#[test]
fn enter_image_turns_the_mmu_off_and_branches_to_its_third_argument() {
    let (params, returns, code) = Stub::EnterImage.code(Arch::Aarch64);
    assert_eq!((params, returns), (3, 0));
    let code = words(&code);
    let clear_m_c_i = [0xD282_00AA, 0x8A2A_0129, 0xD518_1009];
    assert!(code.windows(3).any(|three| three == clear_m_c_i));
    assert_eq!(code.last(), Some(&0xD61F_0040), "it ends with br x2");
}

#[test]
fn pvh_trampoline_enters_with_its_own_flat_segments() {
    let gdt = PVH_TRAMPOLINE.len() - 10 - 24;
    let descriptor = |index: usize| {
        let at = gdt + 8 * index;
        u64::from_le_bytes(PVH_TRAMPOLINE[at..at + 8].try_into().expect("8 bytes"))
    };
    assert_eq!(gdt % 8, 0);
    assert_eq!(
        descriptor(1),
        0x00CF_9A00_0000_FFFF,
        "selector 0x08 is 32-bit code"
    );
    assert_eq!(
        descriptor(2),
        0x00CF_9200_0000_FFFF,
        "selector 0x10 is data"
    );
    let relative = |at: usize| {
        let displacement =
            i32::from_le_bytes(PVH_TRAMPOLINE[at..at + 4].try_into().expect("4 bytes"));
        (at as i64 + 4 + i64::from(displacement)) as usize
    };
    assert_eq!(PVH_TRAMPOLINE[0x0d..0x10], [0x48, 0x8d, 0x05]);
    assert_eq!(relative(0x10), gdt, "the descriptor table's address");
    assert_eq!(PVH_TRAMPOLINE[0x1b..0x1e], [0x0f, 0x01, 0x15]);
    assert_eq!(relative(0x1e), gdt + 24, "lgdt loads the pseudo-descriptor");
    assert!(
        PVH_TRAMPOLINE.windows(2).any(|pair| pair == [0x89, 0xfb]),
        "ebx holds the start information"
    );
    assert!(
        PVH_TRAMPOLINE.windows(2).any(|pair| pair == [0xff, 0xe6]),
        "it jumps to the entry in esi"
    );
}
