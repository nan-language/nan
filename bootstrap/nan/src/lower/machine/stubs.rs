//! The out-of-line compiler stubs of the machine targets: fixed machine code each image emits
//! once for the operations Cranelift cannot express.

use cranelift_codegen::ir::{AbiParam, InstBuilder, Signature, Value};
use cranelift_module::{Linkage, Module};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};

/// The compiler stubs. Each takes its arguments in `rdi`, `rsi` and `rdx`, gives its result in `rax`
/// and returns, but `Halt`, which stops the processor with maskable interrupts disabled. A port
/// stub takes the port and, for `Out`, the value; a memory-mapped stub takes the address and,
/// for a write, the value; `Copy` and `Fill` take the target, the source or byte, and the length,
/// a copy between overlapping ranges running backwards when its target lies above its source.
/// `Thread` reads the first word of the running CPU's record, which the GS base addresses;
/// `Wrmsr` and `Rdmsr` write and read a model-specific register; `LoadTables` loads a CPU's
/// descriptor tables through their pseudo-descriptor, reloading its segments and task register;
/// `EnableAvx` enables AVX and its state saving when the CPU has both, giving whether it did;
/// `IdleWait` halts until an interrupt with maskable interrupts enabled.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Stub {
    Out1,
    Out2,
    Out4,
    In1,
    In2,
    In4,
    MmioRead1,
    MmioRead2,
    MmioRead4,
    MmioRead8,
    MmioWrite1,
    MmioWrite2,
    MmioWrite4,
    MmioWrite8,
    Fill,
    Copy,
    Pause,
    Mfence,
    IdleWait,
    Lidt,
    Sti,
    Cli,
    IrqSave,
    IrqRestore,
    Thread,
    Wrmsr,
    Rdmsr,
    ReadCr2,
    ReadCr3,
    WriteCr3,
    Invlpg,
    LoadTables,
    EnableAvx,
    Halt,
}

impl Stub {
    /// The words the stub takes and gives, and its machine code.
    fn code(self) -> (usize, usize, &'static [u8]) {
        match self {
            Stub::Out1 => (2, 0, &[0x89, 0xfa, 0x89, 0xf0, 0xee, 0xc3]),
            Stub::Out2 => (2, 0, &[0x89, 0xfa, 0x89, 0xf0, 0x66, 0xef, 0xc3]),
            Stub::Out4 => (2, 0, &[0x89, 0xfa, 0x89, 0xf0, 0xef, 0xc3]),
            Stub::In1 => (1, 1, &[0x89, 0xfa, 0x31, 0xc0, 0xec, 0xc3]),
            Stub::In2 => (1, 1, &[0x89, 0xfa, 0x31, 0xc0, 0x66, 0xed, 0xc3]),
            Stub::In4 => (1, 1, &[0x89, 0xfa, 0xed, 0xc3]),
            Stub::MmioRead1 => (1, 1, &[0x0f, 0xb6, 0x07, 0xc3]),
            Stub::MmioRead2 => (1, 1, &[0x0f, 0xb7, 0x07, 0xc3]),
            Stub::MmioRead4 => (1, 1, &[0x8b, 0x07, 0xc3]),
            Stub::MmioRead8 => (1, 1, &[0x48, 0x8b, 0x07, 0xc3]),
            Stub::MmioWrite1 => (2, 0, &[0x40, 0x88, 0x37, 0xc3]),
            Stub::MmioWrite2 => (2, 0, &[0x66, 0x89, 0x37, 0xc3]),
            Stub::MmioWrite4 => (2, 0, &[0x89, 0x37, 0xc3]),
            Stub::MmioWrite8 => (2, 0, &[0x48, 0x89, 0x37, 0xc3]),
            Stub::Fill => (3, 0, &[0x48, 0x89, 0xd1, 0x89, 0xf0, 0xf3, 0xaa, 0xc3]),
            Stub::Copy => (
                3,
                0,
                &[
                    0x48, 0x89, 0xd1, 0x48, 0x39, 0xf7, 0x76, 0x18, 0x48, 0x8d, 0x04, 0x16, 0x48,
                    0x39, 0xc7, 0x73, 0x0f, 0x48, 0x8d, 0x74, 0x16, 0xff, 0x48, 0x8d, 0x7c, 0x17,
                    0xff, 0xfd, 0xf3, 0xa4, 0xfc, 0xc3, 0xf3, 0xa4, 0xc3,
                ],
            ),
            Stub::Pause => (0, 0, &[0xf3, 0x90, 0xc3]),
            Stub::Mfence => (0, 0, &[0x0f, 0xae, 0xf0, 0xc3]),
            Stub::IdleWait => (0, 0, &[0xfb, 0xf4, 0xfa, 0xc3]),
            Stub::Lidt => (1, 0, &[0x0f, 0x01, 0x1f, 0xc3]),
            Stub::Sti => (0, 0, &[0xfb, 0xc3]),
            Stub::Cli => (0, 0, &[0xfa, 0xc3]),
            Stub::IrqSave => (0, 1, &[0x9c, 0x58, 0xfa, 0xc3]),
            Stub::IrqRestore => (
                1,
                0,
                &[0xf7, 0xc7, 0x00, 0x02, 0x00, 0x00, 0x74, 0x01, 0xfb, 0xc3],
            ),
            Stub::Thread => (
                0,
                1,
                &[0x65, 0x48, 0x8b, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00, 0xc3],
            ),
            Stub::Wrmsr => (
                2,
                0,
                &[
                    0x89, 0xf9, 0x48, 0x89, 0xf0, 0x48, 0x89, 0xf2, 0x48, 0xc1, 0xea, 0x20, 0x0f,
                    0x30, 0xc3,
                ],
            ),
            Stub::Rdmsr => (
                1,
                1,
                &[
                    0x89, 0xf9, 0x0f, 0x32, 0x48, 0xc1, 0xe2, 0x20, 0x48, 0x09, 0xd0, 0xc3,
                ],
            ),
            Stub::ReadCr2 => (0, 1, &[0x0f, 0x20, 0xd0, 0xc3]),
            Stub::ReadCr3 => (0, 1, &[0x0f, 0x20, 0xd8, 0xc3]),
            Stub::WriteCr3 => (1, 0, &[0x0f, 0x22, 0xdf, 0xc3]),
            Stub::Invlpg => (1, 0, &[0x0f, 0x01, 0x3f, 0xc3]),
            Stub::LoadTables => (
                1,
                0,
                &[
                    0x0f, 0x01, 0x17, 0x6a, 0x08, 0x48, 0x8d, 0x05, 0x03, 0x00, 0x00, 0x00, 0x50,
                    0x48, 0xcb, 0xb8, 0x10, 0x00, 0x00, 0x00, 0x8e, 0xd8, 0x8e, 0xc0, 0x8e, 0xd0,
                    0xb8, 0x28, 0x00, 0x00, 0x00, 0x0f, 0x00, 0xd8, 0xc3,
                ],
            ),
            Stub::EnableAvx => (
                0,
                1,
                &[
                    0x53, 0xb8, 0x01, 0x00, 0x00, 0x00, 0x0f, 0xa2, 0x5b, 0x81, 0xe1, 0x00, 0x00,
                    0x00, 0x14, 0x81, 0xf9, 0x00, 0x00, 0x00, 0x14, 0x75, 0x1d, 0x0f, 0x20, 0xe0,
                    0x48, 0x0d, 0x00, 0x00, 0x04, 0x00, 0x0f, 0x22, 0xe0, 0x31, 0xc9, 0x0f, 0x01,
                    0xd0, 0x83, 0xc8, 0x07, 0x0f, 0x01, 0xd1, 0xb8, 0x01, 0x00, 0x00, 0x00, 0xc3,
                    0x31, 0xc0, 0xc3,
                ],
            ),
            Stub::Halt => (0, 0, &[0xfa, 0xf4, 0xeb, 0xfd]),
        }
    }
}

impl Lowering<'_, '_> {
    /// Calls a compiler stub, emitted once per image, giving its results.
    pub fn call_stub(&mut self, fx: &mut Fx, stub: Stub, args: &[Value]) -> Vec<Value> {
        let id = match self.machine_stubs.get(&stub) {
            Some(id) => *id,
            None => {
                let (params, returns, bytes) = stub.code();
                let mut signature = Signature::new(super::super::CALL_CONV);
                signature
                    .params
                    .extend((0..params).map(|_| AbiParam::new(PTR)));
                signature
                    .returns
                    .extend((0..returns).map(|_| AbiParam::new(PTR)));
                let id = self
                    .module
                    .declare_function(&format!("_NSstub_{stub:?}"), Linkage::Local, &signature)
                    .expect("a compiler stub");
                self.module
                    .define_function_bytes(id, 1, bytes, &[])
                    .expect("a compiler stub");
                self.machine_stubs.insert(stub, id);
                id
            }
        };
        let reference = self.func_ref(fx, id);
        let call = fx.b.ins().call(reference, args);
        fx.b.inst_results(call).to_vec()
    }

    /// Reads a port through the compiler stub of a width, zero-extended.
    pub fn port_in(&mut self, fx: &mut Fx, width: u64, port: Value) -> Value {
        let stub = match width {
            1 => Stub::In1,
            2 => Stub::In2,
            _ => Stub::In4,
        };
        self.call_stub(fx, stub, &[port])[0]
    }

    /// Writes `value` to a port through the compiler stub of a width.
    pub fn port_out(&mut self, fx: &mut Fx, width: u64, port: Value, value: Value) {
        let stub = match width {
            1 => Stub::Out1,
            2 => Stub::Out2,
            _ => Stub::Out4,
        };
        self.call_stub(fx, stub, &[port, value]);
    }

    /// Loads a value of a width at a memory-mapped address through its compiler stub,
    /// zero-extended.
    pub fn mmio_read(&mut self, fx: &mut Fx, width: u64, address: Value) -> Value {
        let stub = match width {
            1 => Stub::MmioRead1,
            2 => Stub::MmioRead2,
            4 => Stub::MmioRead4,
            _ => Stub::MmioRead8,
        };
        self.call_stub(fx, stub, &[address])[0]
    }

    /// Stores `value` at a memory-mapped address through the compiler stub of a width.
    pub fn mmio_write(&mut self, fx: &mut Fx, width: u64, address: Value, value: Value) {
        let stub = match width {
            1 => Stub::MmioWrite1,
            2 => Stub::MmioWrite2,
            4 => Stub::MmioWrite4,
            _ => Stub::MmioWrite8,
        };
        self.call_stub(fx, stub, &[address, value]);
    }

    /// Writes `value` to the model-specific register `msr`.
    pub fn write_msr(&mut self, fx: &mut Fx, msr: u32, value: Value) {
        let msr = fx.ptr(u64::from(msr));
        self.call_stub(fx, Stub::Wrmsr, &[msr, value]);
    }
}
