//! The run check of a codegen level: an image of a level first checks that its processor has the
//! level's extensions, and on a host target that Linux enables the register state they use,
//! ending with status 71 when one is missing.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::{InstBuilder, Value, types};

use super::Lowering;
use super::fx::{Fx, PTR};
use super::machine::Stub;
use super::runtime::system;
use crate::workspace::Level;

/// A bit set in a register a CPUID leaf gives: the leaf, the register's index among `eax`,
/// `ebx`, `ecx` and `edx`, and the bit.
type CpuidBit = (u32, u64, u32);

/// The CPUID leaves holding the extensions' bits, read below the highest leaf of their range.
const BASIC: u32 = 0;
const FEATURES: u32 = 1;
const EXTENDED_FEATURES: u32 = 7;
const EXTENDED: u32 = 0x8000_0000;
const EXTENDED_SIGNATURE: u32 = 0x8000_0001;

const EBX: u64 = 1;
const ECX: u64 = 2;

/// The bits of `x86_64-v2`: SSE3, SSSE3, CMPXCHG16B, SSE4.1, SSE4.2 and POPCNT.
const V2: [CpuidBit; 6] = [
    (FEATURES, ECX, 0),
    (FEATURES, ECX, 9),
    (FEATURES, ECX, 13),
    (FEATURES, ECX, 19),
    (FEATURES, ECX, 20),
    (FEATURES, ECX, 23),
];

/// Those `x86_64-v3` adds: FMA, AVX, BMI1, AVX2, BMI2 and LZCNT.
const V3: [CpuidBit; 6] = [
    (FEATURES, ECX, 12),
    (FEATURES, ECX, 28),
    (EXTENDED_FEATURES, EBX, 3),
    (EXTENDED_FEATURES, EBX, 5),
    (EXTENDED_FEATURES, EBX, 8),
    (EXTENDED_SIGNATURE, ECX, 5),
];

/// Those `x86_64-v4` adds: AVX-512F, AVX-512DQ and AVX-512VL.
const V4: [CpuidBit; 3] = [
    (EXTENDED_FEATURES, EBX, 16),
    (EXTENDED_FEATURES, EBX, 17),
    (EXTENDED_FEATURES, EBX, 31),
];

/// The `XCR0` state components AVX uses, SSE and AVX, and those AVX-512 adds, the opmask and
/// the upper halves of the first sixteen registers and the sixteen others.
const AVX_STATE: i64 = 0x6;
const AVX512_STATE: i64 = 0xE6;

/// The `getauxval` type of the hardware capabilities, and their AArch64 bits for LSE atomics and
/// the dot product instructions.
const AT_HWCAP: u64 = 16;
const HWCAP_ATOMICS: i64 = 1 << 8;
const HWCAP_ASIMDDP: i64 = 1 << 20;

/// The encoding of `ID_AA64ISAR0_EL1` for the `ReadSysReg` stub, and its fields: atomics at bit
/// 20, at least 2 with LSE, and dot product at bit 44, at least 1 with it.
const ID_AA64ISAR0_EL1: u16 = 0x4030;
const ATOMIC_FIELD: i64 = 20;
const DP_FIELD: i64 = 44;

impl Lowering<'_, '_> {
    /// Checks the image's codegen level before anything else runs, writing
    /// `nan: codegen <level> unsupported` and ending with status 71 when the processor lacks it.
    pub fn check_level(&mut self, fx: &mut Fx) {
        let Some(level) = self.level else {
            return;
        };
        let missing = match level {
            Level::Aarch64Dotprod if self.machine() => {
                let features = self.call_stub(fx, Stub::ReadSysReg(ID_AA64ISAR0_EL1), &[])[0];
                let atomic = field(fx, features, ATOMIC_FIELD);
                let dot = field(fx, features, DP_FIELD);
                let no_atomic = fx.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, atomic, 2);
                let no_dot = fx.b.ins().icmp_imm_s(IntCC::Equal, dot, 0);
                fx.b.ins().bor(no_atomic, no_dot)
            }
            Level::Aarch64Dotprod => {
                let kind = fx.ptr(AT_HWCAP);
                let hwcap = self.call_libc(fx, "getauxval", &[kind], &[PTR])[0];
                lacks(fx, hwcap, HWCAP_ATOMICS | HWCAP_ASIMDDP)
            }
            _ => self.x64_missing(fx, level),
        };
        fx.when(missing, |fx| {
            let why = format!("nan: codegen {} unsupported\n", level.name());
            system::refuse(self, fx, &why);
        });
    }

    /// Whether the processor lacks an extension of an x86_64 level, or on a host target Linux
    /// leaves the register state of its AVX or AVX-512 extensions disabled.
    fn x64_missing(&mut self, fx: &mut Fx, level: Level) -> Value {
        let mut bits = V2.to_vec();
        if level != Level::X64V2 {
            bits.extend(V3);
        }
        if level == Level::X64V4 {
            bits.extend(V4);
        }
        let registers = fx.slot(16, 4);
        let mut missing = fx.b.ins().iconst(types::I8, 0);
        let highest = [BASIC, EXTENDED].map(|range| self.cpuid(fx, registers, range, 0));
        for leaf in [FEATURES, EXTENDED_FEATURES, EXTENDED_SIGNATURE] {
            let wanted = bits
                .iter()
                .filter(|(at, _, _)| *at == leaf)
                .collect::<Vec<_>>();
            if wanted.is_empty() {
                continue;
            }
            let range = usize::from(leaf >= EXTENDED);
            let beyond =
                fx.b.ins()
                    .icmp_imm_s(IntCC::UnsignedLessThan, highest[range], i64::from(leaf));
            missing = fx.b.ins().bor(missing, beyond);
            self.cpuid(fx, registers, leaf, 0);
            for register in [EBX, ECX] {
                let mask = wanted
                    .iter()
                    .filter(|(_, at, _)| *at == register)
                    .fold(0i64, |mask, (_, _, bit)| mask | 1 << bit);
                if mask != 0 {
                    let word = fx.load(types::I32, registers, register * 4);
                    let word = fx.b.ins().uextend(types::I64, word);
                    let lacking = lacks(fx, word, mask);
                    missing = fx.b.ins().bor(missing, lacking);
                }
            }
        }
        if !self.machine() {
            let state = match level {
                Level::X64V2 => 0,
                Level::X64V4 => AVX512_STATE,
                _ => AVX_STATE,
            };
            if state != 0 {
                let xcr0 = self.call_stub(fx, Stub::Xgetbv, &[])[0];
                let disabled = lacks(fx, xcr0, state);
                missing = fx.b.ins().bor(missing, disabled);
            }
        }
        missing
    }

    /// Runs CPUID for a leaf and subleaf into the four words at `registers`, giving `eax`
    /// widened.
    fn cpuid(&mut self, fx: &mut Fx, registers: Value, leaf: u32, subleaf: u32) -> Value {
        let leaf = fx.ptr(u64::from(leaf));
        let subleaf = fx.ptr(u64::from(subleaf));
        self.call_stub(fx, Stub::Cpuid, &[leaf, subleaf, registers]);
        let eax = fx.load(types::I32, registers, 0);
        fx.b.ins().uextend(types::I64, eax)
    }
}

/// Whether some bit of `mask` is clear in `word`.
fn lacks(fx: &mut Fx, word: Value, mask: i64) -> Value {
    let held = fx.b.ins().band_imm_s(word, mask);
    fx.b.ins().icmp_imm_s(IntCC::NotEqual, held, mask)
}

/// The four-bit field of an AArch64 ID register at a bit.
fn field(fx: &mut Fx, register: Value, at: i64) -> Value {
    let shifted = fx.b.ins().ushr_imm_s(register, at);
    fx.b.ins().band_imm_s(shifted, 0xF)
}
