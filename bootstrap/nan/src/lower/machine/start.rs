//! The start information a bare image may enter with, in the layout of the PVH boot protocol's:
//! its magic number, module list, command line, RSDP and memory map.

use cranelift_codegen::ir::{InstBuilder, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::state;
use crate::workspace::Target;

/// The offsets in a start information of its magic number, version, module count, module list's
/// address, command line's address, RSDP's address, memory map's address and memory map's entry
/// count, which version 1 adds; its size and its magic number.
pub mod info {
    pub const MAGIC: u64 = 0;
    pub const VERSION: u64 = 4;
    pub const MODULE_COUNT: u64 = 12;
    pub const MODULES: u64 = 16;
    pub const COMMAND_LINE: u64 = 24;
    pub const RSDP: u64 = 32;
    pub const MAP: u64 = 40;
    pub const MAP_COUNT: u64 = 48;
    pub const BYTES: i64 = 56;
    pub const MAGIC_NUMBER: i64 = 0x336E_C578;
}

/// The offsets in a module list entry of the module's address, size and NUL-terminated name, and
/// an entry's size.
pub mod module {
    pub const ADDRESS: u64 = 0;
    pub const SIZE: u64 = 8;
    pub const NAME: u64 = 16;
    pub const BYTES: i64 = 32;
}

/// The offsets in a memory map entry of its address, size and type, an entry's size, and the
/// type of usable memory.
pub mod map_entry {
    pub const BASE: u64 = 0;
    pub const SIZE: u64 = 8;
    pub const KIND: u64 = 16;
    pub const BYTES: i64 = 24;
    pub const USABLE: i64 = 1;
}

impl Lowering<'_, '_> {
    /// The address of the start information the image entered with, zero when it entered with
    /// none: the PVH protocol's on `x86_64-bare` and the one in `x1` on `aarch64-bare`, each when
    /// it holds the magic number.
    pub fn start_info(&mut self, fx: &mut Fx) -> Value {
        let given = match self.ck.db.target {
            Target::X64Bare => state::PLATFORM,
            Target::Aarch64Bare => state::PLATFORM_TABLE,
            _ => return fx.ptr(0),
        };
        let state_address = self.state(fx);
        let candidate = fx.load(PTR, state_address, given);
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, candidate, 0);
        fx.choose(
            present,
            &[PTR],
            |fx| {
                let magic = fx.load(types::I32, candidate, info::MAGIC);
                let valid =
                    fx.b.ins()
                        .icmp_imm_s(IntCC::Equal, magic, info::MAGIC_NUMBER);
                let zero = fx.ptr(0);
                vec![fx.b.ins().select(valid, candidate, zero)]
            },
            |fx| vec![fx.ptr(0)],
        )[0]
    }
}

/// The 32-bit count at an offset of a start information, zero-extended.
pub fn count(fx: &mut Fx, info: Value, offset: u64) -> Value {
    let count = fx.load(types::I32, info, offset);
    fx.b.ins().uextend(PTR, count)
}

/// The address past the NUL ending the text at an address, the address itself when it is zero.
pub fn text_end(fx: &mut Fx, text: Value) -> Value {
    let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, text, 0);
    fx.choose(
        present,
        &[PTR],
        |fx| {
            fx.repeat(&[text], |fx, state| {
                let byte = fx.load(types::I8, state[0], 0);
                let next = fx.b.ins().iadd_imm_s(state[0], 1);
                let more = fx.b.ins().icmp_imm_s(IntCC::NotEqual, byte, 0);
                (more, vec![next])
            })
        },
        |_| vec![text],
    )[0]
}
