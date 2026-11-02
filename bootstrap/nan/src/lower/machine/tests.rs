use super::aarch64::{self, VBAR_SEAM};
use super::handoff::PVH_TRAMPOLINE;
use super::interrupts::{
    CHECK, CHECKS_AT, COMMON_AT, RESERVED, SEAM_CHECKS, SEAM_COMMON, SEAM_DISPATCH, SEAM_PREEMPT,
    SEAM_STAY, USER_AT,
};
use super::stubs::Stub;
use super::user;
use super::{
    AARCH64_BARE_SEAM, AARCH64_SEAM_BOOT, AARCH64_SEAM_RELOCATIONS, AARCH64_SEAM_ROOT,
    AARCH64_SEAM_STACK, AARCH64_SECONDARY_SEAM,
};
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

/// The target of the x86-64 jump whose 32-bit displacement lies at `at` in code placed at `base`.
fn x64_target(code: &[u8], base: u32, at: u32) -> u32 {
    let displacement = &code[at as usize..at as usize + 4];
    let displacement = i32::from_le_bytes(displacement.try_into().expect("four bytes"));
    (base + at + 4).wrapping_add_signed(displacement)
}

#[test]
fn x64_user_path_meets_the_vector_seam() {
    for jump in [7, 0x14] {
        assert_eq!(
            x64_target(SEAM_CHECKS, CHECKS_AT, jump),
            USER_AT + user::X64_VECTOR,
            "the check's jump at {jump:#x} reaches the user path's entry for vectors"
        );
    }
    let stay = CHECKS_AT + CHECK + u32::from(SEAM_CHECKS[CHECK as usize - 1]);
    assert_eq!(stay, COMMON_AT + SEAM_STAY, "the first check's short jump");
    assert_eq!(
        x64_target(user::X64_USER, USER_AT, user::X64_COMMON_JUMP),
        COMMON_AT,
        "the user path's interrupts enter the common seam"
    );
    assert!(!saves_extended_state(user::X64_USER));
}

/// The target of the AArch64 `b` or `bl` at `at`.
fn a64_branch_target(word: u32, at: usize) -> usize {
    let words = ((word << 6) as i32) >> 6;
    at.wrapping_add_signed(words as isize * 4)
}

#[test]
fn aarch64_user_path_meets_the_vector_table() {
    let table = words(VBAR_SEAM);
    for kind in 0..4 {
        let at = 0x408 + 0x80 * kind;
        assert_eq!(
            a64_branch_target(table[at / 4], at),
            aarch64::USER_AT as usize + user::AARCH64_ENTRY as usize,
            "the EL0 entry of kind {kind} branches to the user path"
        );
    }
    let path = words(user::AARCH64_USER);
    for (from, to) in user::AARCH64_KERNEL_ENTRIES {
        let at = aarch64::USER_AT as usize + from as usize;
        assert_eq!(a64_branch_target(path[from as usize / 4], at), to as usize);
    }
    for word in &path {
        assert!(
            !touches_floats(*word),
            "{word:#010x} touches the floating-point registers"
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
        entries, 8,
        "each entry for an exception taken at EL1 opens a frame of {bytes} bytes"
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
    let saved_sp = 0xF940_0001 | (aarch64::frame::SP as u32 / 8) << 10;
    let msr_sp_el0 = 0xD518_4101;
    assert!(
        code.windows(2).any(|pair| pair == [saved_sp, msr_sp_el0]),
        "the return from preemption restores the stack pointer the copy holds"
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
        Stub::Smc,
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

/// An AArch64 `MSR` or `MRS` of the system register its fields name, from or into `x9`.
fn system_register(read: bool, op0: u32, op1: u32, crn: u32, crm: u32, op2: u32) -> u32 {
    let base = if read { 0xD530_0000 } else { 0xD510_0000 };
    base | (op0 - 2) << 19 | op1 << 16 | crn << 12 | crm << 8 | op2 << 5 | 9
}

/// The instructions both AArch64 seams begin with, which drop from EL2 to EL1.
const EL2_DROP: usize = 29;

#[test]
fn aarch64_seams_drop_from_el2_to_el1_alike() {
    let bare = words(AARCH64_BARE_SEAM);
    let secondary = words(AARCH64_SECONDARY_SEAM);
    assert_eq!(bare[..EL2_DROP], secondary[..EL2_DROP]);
    let drop = &bare[..EL2_DROP];
    let current_el = system_register(true, 3, 0, 4, 2, 2);
    assert_eq!(drop[0], current_el);
    assert_eq!(drop[1], 0xF100_213F, "cmp x9, #8: entered at EL2");
    assert_eq!(
        a64_condition_target(drop[2], 2),
        EL2_DROP * 4,
        "b.ne past the drop at EL1"
    );
    let writes = |op1, crn, crm, op2| drop.contains(&system_register(false, 3, op1, crn, crm, op2));
    let hcr = drop
        .iter()
        .position(|word| *word == system_register(false, 3, 4, 1, 1, 0));
    assert_eq!(
        drop[hcr.expect("an HCR_EL2 write") - 1],
        0xD2B0_0009,
        "HCR_EL2 holds RW alone"
    );
    for (name, register) in [
        ("CNTHCTL_EL2", (4, 14, 1, 0)),
        ("ICC_SRE_EL2", (4, 12, 9, 5)),
        ("CPTR_EL2", (4, 1, 1, 2)),
        ("VPIDR_EL2", (4, 0, 0, 0)),
        ("VMPIDR_EL2", (4, 0, 0, 5)),
        ("SCTLR_EL1", (0, 1, 0, 0)),
        ("SPSR_EL2", (4, 4, 0, 0)),
        ("ELR_EL2", (4, 4, 0, 1)),
    ] {
        let (op1, crn, crm, op2) = register;
        assert!(writes(op1, crn, crm, op2), "the drop writes {name}");
    }
    assert!(drop.contains(&0xD51C_E07F), "CNTVOFF_EL2 is zero");
    assert!(drop.contains(&0xD51C_117F), "HSTR_EL2 is zero");
    assert!(drop.contains(&0xD286_7FE9), "CPTR_EL2 traps nothing");
    assert!(
        drop.contains(&0xD280_78A9),
        "SPSR_EL2 returns to EL1h masked"
    );
    let adr = drop[EL2_DROP - 4];
    assert_eq!(adr & 0x9F00_001F, 0x1000_0009, "adr x9");
    let offset = ((adr >> 29 & 3) | (adr >> 5 & 0x7_FFFF) << 2) as usize;
    assert_eq!(
        (EL2_DROP - 4) * 4 + offset,
        EL2_DROP * 4,
        "ELR_EL2 resumes after the drop"
    );
    assert_eq!(drop[EL2_DROP - 1], 0xD69F_03E0, "eret");
}

/// The byte offset a `b.cond` at the instruction index `at` reaches.
fn a64_condition_target(word: u32, at: usize) -> usize {
    a64_target(word, at * 4)
}

#[test]
fn aarch64_bare_seam_maps_the_image_relocates_it_and_passes_x0_and_x1_on() {
    let code = words(AARCH64_BARE_SEAM);
    for (offset, register) in [(AARCH64_SEAM_RELOCATIONS, 2), (AARCH64_SEAM_ROOT, 13)] {
        let at = offset as usize / 4;
        assert_eq!(code[at], 0x9000_0000 | register, "adrp at {offset:#x}");
        assert_eq!(
            code[at + 1],
            0x9100_0000 | register << 5 | register,
            "add of the low bits at {:#x}",
            offset + 4
        );
    }
    let pool: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, word)| *word & 0xFF00_0000 == 0x5800_0000)
        .map(|(index, word)| a64_target(*word, index * 4))
        .collect();
    for offset in [AARCH64_SEAM_STACK, AARCH64_SEAM_BOOT] {
        assert_eq!(offset % 8, 0, "the literal at {offset:#x} is aligned");
        assert!(
            pool.contains(&(offset as usize)),
            "an ldr reads {offset:#x}"
        );
    }
    let normal = pool
        .iter()
        .map(|at| u64::from_le_bytes(AARCH64_BARE_SEAM[*at..*at + 8].try_into().expect("8 bytes")))
        .find(|value| *value == super::A64_NORMAL_BLOCK);
    assert!(normal.is_some(), "the pool holds a normal block's bits");
    let stores: Vec<usize> = code
        .iter()
        .enumerate()
        .filter(|(_, word)| *word & 0xFFC0_0000 == 0xF900_0000)
        .map(|(index, _)| index)
        .collect();
    let mmu_on = code
        .iter()
        .rposition(|word| *word == 0xD518_1009)
        .expect("an SCTLR_EL1 write");
    let early: Vec<&usize> = stores.iter().filter(|store| **store < mmu_on).collect();
    assert_eq!(
        early.len(),
        2,
        "the level-0 entry and each gigabyte's are written early"
    );
    for store in early {
        let base = code[*store] >> 5 & 0x1F;
        assert_eq!(
            code[store + 1],
            0xD508_7620 | base,
            "dc ivac follows a table write with the MMU off"
        );
    }
    let relocation = code
        .iter()
        .position(|word| *word == 0xF840_8486)
        .expect("ldr x6, [x4], #8 reads the table");
    assert!(relocation > mmu_on, "relocation runs with the MMU on");
    assert_eq!(
        code[relocation + 1],
        0x8B03_00C6,
        "add x6, x6, x3: the loaded word"
    );
    assert_eq!(
        code[relocation + 3],
        0x8B03_00E7,
        "add x7, x7, x3: its loaded value"
    );
    let call = code
        .iter()
        .position(|word| *word == 0xD63F_0120)
        .expect("a blr x9");
    let writes = |register: u32| {
        code[..call]
            .iter()
            .filter(|word| {
                let writing = !matches!(**word >> 24, 0x54 | 0xB4 | 0xB5 | 0xD5 | 0xD6 | 0xF9);
                **word & 0x1F == register && writing && **word >> 26 != 0b00_0101
            })
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
