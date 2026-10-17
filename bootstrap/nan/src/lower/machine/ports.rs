//! The ports the machine descriptors name: the 16550 serial port reports and test frames are
//! written to, the exit port that ends the emulator, and the emulator's configuration device,
//! from which a test image reads the texts naming its run.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, system};
use super::stubs::Stub;

/// The serial and exit ports the descriptor names for a machine target.
const SERIAL_PORT: u64 = 0x3F8;
const EXIT_PORT: u64 = 0xF4;

/// The writes programming the serial port's UART, each a register's offset and its value: no
/// interrupts, the divisor latch open, divisor 1, then 8N1 with the latch closed, the FIFOs on
/// and cleared, and the modem lines raised with loopback off.
const SERIAL_SETUP: [(u64, u64); 7] = [
    (1, 0x00),
    (3, 0x80),
    (0, 0x01),
    (1, 0x00),
    (3, 0x03),
    (2, 0xC7),
    (4, 0x03),
];

/// The UART's line status register and its bit set while the transmitter holds no byte.
const LINE_STATUS: u64 = 5;
const TRANSMITTER_EMPTY: i64 = 0x20;

impl Lowering<'_, '_> {
    /// Programs the 16550 UART at the serial port: 115200 baud, 8 data bits, no parity and one
    /// stop bit, its FIFOs on, its interrupts and loopback off.
    pub fn serial_init(&mut self, fx: &mut Fx) {
        for (register, value) in SERIAL_SETUP {
            let port = fx.ptr(SERIAL_PORT + register);
            let value = fx.ptr(value);
            self.port_out(fx, 1, port, value);
        }
    }

    /// Writes a byte range to the 16550 serial port, each byte once its transmitter holds none.
    pub fn serial_write(&mut self, fx: &mut Fx, text: Value, len: Value) {
        let port = fx.ptr(SERIAL_PORT);
        let status = fx.ptr(SERIAL_PORT + LINE_STATUS);
        let start = fx.ptr(0);
        fx.each(start, len, |fx, index| {
            fx.repeat(&[], |fx, _| {
                let line = self.port_in(fx, 1, status);
                let empty = fx.b.ins().band_imm_s(line, TRANSMITTER_EMPTY);
                let busy = fx.b.ins().icmp_imm_s(IntCC::Equal, empty, 0);
                (busy, Vec::new())
            });
            let at = fx.b.ins().iadd(text, index);
            let byte =
                fx.b.ins()
                    .load(types::I8, super::super::fx::unaligned(), at, 0);
            let byte = fx.b.ins().uextend(PTR, byte);
            self.port_out(fx, 1, port, byte);
        });
    }

    /// Ends the machine through the exit port, which the emulator reads, then stops the processor
    /// with maskable interrupts disabled; `status` is the byte written. Nothing follows.
    pub fn exit_machine(&mut self, fx: &mut Fx, status: Value) {
        let port = fx.ptr(EXIT_PORT);
        self.port_out(fx, 1, port, status);
        self.call_stub(fx, Stub::Halt, &[]);
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    }
}

/// The emulator's configuration device: its selector and data ports, the selector of its file
/// directory, and the bytes of a directory entry, which holds the file's size and selector
/// big-endian, two reserved bytes and its NUL-padded name.
const FW_CFG_SELECTOR: u64 = 0x510;
const FW_CFG_DATA: u64 = 0x511;
const FW_CFG_FILE_DIR: u64 = 0x19;
const FW_CFG_NAME: u64 = 56;

impl Lowering<'_, '_> {
    /// Reads the emulator's configuration file of a name into a runtime record: its address and
    /// length, a NUL following its bytes; the length is zero when the file is absent.
    pub fn fw_cfg_file(&mut self, fx: &mut Fx, name: &str) -> (Value, Value) {
        let selector = fx.ptr(FW_CFG_SELECTOR);
        let directory = fx.ptr(FW_CFG_FILE_DIR);
        self.port_out(fx, 2, selector, directory);
        let count = self.fw_cfg_number(fx, 4);
        let mut wanted = name.as_bytes().to_vec();
        wanted.push(0);
        let wanted_len = fx.ptr(wanted.len() as u64);
        let view = self.view_literal(fx, &wanted).address();
        let wanted = fx.load(PTR, view, 0);
        let entry = fx.slot(FW_CFG_NAME, 8);
        let zero = fx.ptr(0);
        let size = fx.var(PTR, zero);
        let select = fx.var(PTR, zero);
        fx.each(zero, count, |fx, _| {
            let file_size = self.fw_cfg_number(fx, 4);
            let file_select = self.fw_cfg_number(fx, 2);
            self.fw_cfg_number(fx, 2);
            let data = fx.ptr(FW_CFG_DATA);
            let start = fx.ptr(0);
            let end = fx.ptr(FW_CFG_NAME);
            fx.each(start, end, |fx, index| {
                let byte = self.port_in(fx, 1, data);
                let byte = fx.b.ins().ireduce(types::I8, byte);
                let at = fx.b.ins().iadd(entry, index);
                fx.store(byte, at, 0);
            });
            let order = self.call_routine(fx, Routine::Compare, &[entry, wanted, wanted_len])[0];
            let same = fx.b.ins().icmp_imm_s(IntCC::Equal, order, 0);
            fx.when(same, |fx| {
                fx.b.def_var(size, file_size);
                fx.b.def_var(select, file_select);
            });
        });
        let size = fx.b.use_var(size);
        let select = fx.b.use_var(select);
        let room = fx.b.ins().iadd_imm_s(size, 1);
        let record = self.record_alloc(fx, room);
        let missing = fx.b.ins().icmp_imm_s(IntCC::Equal, record, 0);
        fx.when(missing, |fx| system::unavailable(self, fx));
        let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, size, 0);
        fx.when(present, |fx| {
            self.port_out(fx, 2, selector, select);
            let data = fx.ptr(FW_CFG_DATA);
            let start = fx.ptr(0);
            fx.each(start, size, |fx, index| {
                let byte = self.port_in(fx, 1, data);
                let byte = fx.b.ins().ireduce(types::I8, byte);
                let at = fx.b.ins().iadd(record, index);
                fx.store(byte, at, 0);
            });
        });
        (record, size)
    }

    /// A big-endian number of `width` bytes read from the configuration device's data port.
    fn fw_cfg_number(&mut self, fx: &mut Fx, width: u64) -> Value {
        let data = fx.ptr(FW_CFG_DATA);
        let mut value = fx.ptr(0);
        for _ in 0..width {
            let byte = self.port_in(fx, 1, data);
            let shifted = fx.b.ins().ishl_imm_s(value, 8);
            value = fx.b.ins().bor(shifted, byte);
        }
        value
    }
}
