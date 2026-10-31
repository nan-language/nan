//! The jump from a UEFI loader into a loaded bare image by its target's boot protocol, once the
//! loader has ended boot services: on x86_64 the PVH protocol, through a trampoline copied below
//! 4 GiB, and on AArch64 the image entry with the MMU and caches off.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, TrapCode, Value};
use cranelift_codegen::isa::CallConv;
use cranelift_module::{DataDescription, Linkage, Module};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::Routine;
use super::super::stack::{Kind, Site};
use super::Stub;
use crate::workspace::Arch;

/// The x86_64 hand-off trampoline, position-independent, entered in long mode with the start
/// information in `rdi` and the image's 32-bit entry in `rsi`: with maskable interrupts disabled
/// and PCIDs off, it loads its own descriptor table, whose address it writes into its
/// pseudo-descriptor, and far-returns to its 32-bit code segment; there it turns paging off,
/// which leaves long mode, clears `EFER`, sets `CR0` to protection alone and clears `CR4`,
/// loads its flat data segment, and jumps to the entry with the start information in `ebx`.
pub(super) const PVH_TRAMPOLINE: &[u8] = &[
    0xfa, 0x0f, 0x20, 0xe0, 0x48, 0x25, 0xff, 0xff, 0xfd, 0xff, 0x0f, 0x22, 0xe0, 0x48, 0x8d, 0x05,
    0x54, 0x00, 0x00, 0x00, 0x48, 0x89, 0x05, 0x67, 0x00, 0x00, 0x00, 0x0f, 0x01, 0x15, 0x5e, 0x00,
    0x00, 0x00, 0x48, 0x8d, 0x05, 0x05, 0x00, 0x00, 0x00, 0x6a, 0x08, 0x50, 0x48, 0xcb, 0x0f, 0x20,
    0xc0, 0x25, 0xff, 0xff, 0xff, 0x7f, 0x0f, 0x22, 0xc0, 0xb9, 0x80, 0x00, 0x00, 0xc0, 0x31, 0xc0,
    0x31, 0xd2, 0x0f, 0x30, 0xb8, 0x11, 0x00, 0x00, 0x00, 0x0f, 0x22, 0xc0, 0x31, 0xc0, 0x0f, 0x22,
    0xe0, 0x66, 0xb8, 0x10, 0x00, 0x8e, 0xd8, 0x8e, 0xc0, 0x8e, 0xd0, 0x8e, 0xe0, 0x8e, 0xe8, 0x89,
    0xfb, 0xff, 0xe6, 0x0f, 0x1f, 0x44, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    0xff, 0xff, 0x00, 0x00, 0x00, 0x9a, 0xcf, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x92, 0xcf, 0x00,
    0x17, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

impl Lowering<'_, '_> {
    /// Enters a loaded bare image at `entry` with the start information at `info` and the device
    /// tree at `tree`: on x86_64 by copying the trampoline to the page at `trampoline` and
    /// running it there, on AArch64 through the `EnterImage` stub, with the device tree in `x0`
    /// and the start information in `x1`. Neither returns.
    pub fn enter_image(
        &mut self,
        fx: &mut Fx,
        entry: Value,
        info: Value,
        tree: Value,
        trampoline: Value,
    ) {
        match self.ck.db.target.arch() {
            Some(Arch::Aarch64) => {
                self.call_stub(fx, Stub::EnterImage, &[tree, info, entry]);
            }
            _ => {
                let code = self.pvh_trampoline();
                let code = self.data_address(fx, code);
                let len = fx.ptr(PVH_TRAMPOLINE.len() as u64);
                self.call_routine(fx, Routine::Copy, &[trampoline, code, len]);
                let mut signature = Signature::new(CallConv::SystemV);
                signature
                    .params
                    .extend([AbiParam::new(PTR), AbiParam::new(PTR)]);
                let signature = fx.b.import_signature(signature);
                let call =
                    fx.b.ins()
                        .call_indirect(signature, trampoline, &[info, entry]);
                fx.sites.push((
                    call,
                    Site {
                        kind: Kind::Stub,
                        at: None,
                    },
                ));
            }
        }
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    }

    /// The x86_64 hand-off trampoline's code, a read-only data object the loader copies.
    fn pvh_trampoline(&mut self) -> cranelift_module::DataId {
        let id = self
            .module
            .declare_data("_NSpvh_trampoline", Linkage::Local, false, false)
            .expect("the hand-off trampoline");
        let mut description = DataDescription::new();
        description.define(PVH_TRAMPOLINE.to_vec().into_boxed_slice());
        description.set_align(8);
        self.module
            .define_data(id, &description)
            .expect("the hand-off trampoline");
        id
    }
}
