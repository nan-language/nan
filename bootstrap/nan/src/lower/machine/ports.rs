//! The ports the machine descriptors name: the serial port reports and test frames are written
//! to, the 16550 on x86_64 and the PL011 on AArch64; the exit port that ends the emulator; and the
//! emulator's configuration device, from which a test image reads the texts naming its run.

use cranelift_codegen::ir::{InstBuilder, TrapCode, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::{Routine, system};
use super::stubs::Stub;
use crate::workspace::Arch;

/// The serial and exit ports the x86_64 descriptors name.
const SERIAL_PORT: u64 = 0x3F8;
const EXIT_PORT: u64 = 0xF4;

/// The PL011 UART the AArch64 descriptors name, and its registers: the data register, the flag
/// register and its transmit-full bit, the integer and fractional baud divisors, the line
/// control register with eight data bits and its FIFOs enabled, and the control register
/// enabling the UART, its transmitter and its receiver.
const PL011: u64 = 0x0900_0000;
const PL011_DATA: u64 = 0x00;
const PL011_FLAG: u64 = 0x18;
const PL011_TRANSMIT_FULL: i64 = 1 << 5;
const PL011_IBRD: u64 = 0x24;
const PL011_FBRD: u64 = 0x28;
const PL011_LINE_CONTROL: u64 = 0x2C;
const PL011_EIGHT_BITS: u64 = 0x70;
const PL011_CONTROL: u64 = 0x30;
const PL011_ENABLE: u64 = 0x301;

/// The writes programming the PL011, each a register's offset and its value: the UART off, the
/// divisors giving 115200 baud from the `virt` board's 24 MHz UART clock, 8N1 with the FIFOs on,
/// then the UART on.
const PL011_SETUP: [(u64, u64); 5] = [
    (PL011_CONTROL, 0),
    (PL011_IBRD, 13),
    (PL011_FBRD, 1),
    (PL011_LINE_CONTROL, PL011_EIGHT_BITS),
    (PL011_CONTROL, PL011_ENABLE),
];

/// The writes programming the 16550, each a register's offset and its value: no
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
    /// Programs the serial port's UART to 115200 baud, 8 data bits, no parity and one stop bit,
    /// its FIFOs on: the 16550 at an x86_64 port, or the PL011 at the AArch64 descriptor's
    /// memory-mapped address.
    pub fn serial_init(&mut self, fx: &mut Fx) {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            for (register, value) in PL011_SETUP {
                let at = fx.ptr(PL011 + register);
                let value = fx.ptr(value);
                self.mmio_write(fx, 4, at, value);
            }
            return;
        }
        for (register, value) in SERIAL_SETUP {
            let port = fx.ptr(SERIAL_PORT + register);
            let value = fx.ptr(value);
            self.port_out(fx, 1, port, value);
        }
    }

    /// Writes a byte range to the serial port, each byte once its transmitter can take one.
    pub fn serial_write(&mut self, fx: &mut Fx, text: Value, len: Value) {
        let aarch64 = self.ck.db.target.arch() == Some(Arch::Aarch64);
        let (data, status, ready) = match aarch64 {
            true => (PL011 + PL011_DATA, PL011 + PL011_FLAG, PL011_TRANSMIT_FULL),
            false => (SERIAL_PORT, SERIAL_PORT + LINE_STATUS, TRANSMITTER_EMPTY),
        };
        let data = fx.ptr(data);
        let status = fx.ptr(status);
        let start = fx.ptr(0);
        fx.each(start, len, |fx, index| {
            fx.repeat(&[], |fx, _| {
                let line = match aarch64 {
                    true => self.mmio_read(fx, 4, status),
                    false => self.port_in(fx, 1, status),
                };
                let flag = fx.b.ins().band_imm_s(line, ready);
                // The x86_64 flag is set when the transmitter is free; the PL011 flag when it is
                // full, so each architecture waits while its own condition on the flag holds.
                let busy = match aarch64 {
                    true => fx.b.ins().icmp_imm_s(IntCC::NotEqual, flag, 0),
                    false => fx.b.ins().icmp_imm_s(IntCC::Equal, flag, 0),
                };
                (busy, Vec::new())
            });
            let at = fx.b.ins().iadd(text, index);
            let byte =
                fx.b.ins()
                    .load(types::I8, super::super::fx::unaligned(), at, 0);
            let byte = fx.b.ins().uextend(PTR, byte);
            match aarch64 {
                true => self.mmio_write(fx, 1, data, byte),
                false => self.port_out(fx, 1, data, byte),
            }
        });
    }

    /// Ends the machine through the exit port the emulator reads, writing the status byte and
    /// then stopping the processor with maskable interrupts disabled. On AArch64 the exit is a
    /// semihosting call whose subcode is twice the status plus one, the exit status the x86_64
    /// exit device gives the emulator. Nothing follows.
    pub fn exit_machine(&mut self, fx: &mut Fx, status: Value) {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            let doubled = fx.b.ins().ishl_imm_s(status, 1);
            let code = fx.b.ins().bor_imm_s(doubled, 1);
            self.call_stub(fx, Stub::SemiExit, &[code]);
            fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
            return;
        }
        let port = fx.ptr(EXIT_PORT);
        self.port_out(fx, 1, port, status);
        self.call_stub(fx, Stub::Halt, &[]);
        fx.b.ins().trap(TrapCode::user(1).expect("a trap code"));
    }
}

/// The emulator's configuration device: its x86_64 selector and data ports, and its AArch64
/// memory-mapped data register and the selector register after it, which takes the selector
/// big-endian; the selector of its file directory, and the bytes of a directory entry, which
/// holds the file's size and selector big-endian, two reserved bytes and its NUL-padded name.
const FW_CFG_SELECTOR: u64 = 0x510;
const FW_CFG_DATA: u64 = 0x511;
const FW_CFG_MMIO_DATA: u64 = 0x0902_0000;
const FW_CFG_MMIO_SELECTOR: u64 = 0x0902_0008;
const FW_CFG_FILE_DIR: u64 = 0x19;
const FW_CFG_NAME: u64 = 56;

impl Lowering<'_, '_> {
    /// Reads the emulator's configuration file of a name into a runtime record: its address and
    /// length, a NUL following its bytes; the length is zero when the file is absent.
    pub fn fw_cfg_file(&mut self, fx: &mut Fx, name: &str) -> (Value, Value) {
        let directory = fx.ptr(FW_CFG_FILE_DIR);
        self.fw_cfg_select(fx, directory);
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
            let start = fx.ptr(0);
            let end = fx.ptr(FW_CFG_NAME);
            fx.each(start, end, |fx, index| {
                let byte = self.fw_cfg_byte(fx);
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
            self.fw_cfg_select(fx, select);
            let start = fx.ptr(0);
            fx.each(start, size, |fx, index| {
                let byte = self.fw_cfg_byte(fx);
                let byte = fx.b.ins().ireduce(types::I8, byte);
                let at = fx.b.ins().iadd(record, index);
                fx.store(byte, at, 0);
            });
        });
        (record, size)
    }

    /// Selects the configuration device's item of a 16-bit selector.
    fn fw_cfg_select(&mut self, fx: &mut Fx, item: Value) {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            let low = fx.b.ins().band_imm_s(item, 0xFF);
            let low = fx.b.ins().ishl_imm_s(low, 8);
            let high = fx.b.ins().ushr_imm_s(item, 8);
            let swapped = fx.b.ins().bor(low, high);
            let at = fx.ptr(FW_CFG_MMIO_SELECTOR);
            self.mmio_write(fx, 2, at, swapped);
            return;
        }
        let selector = fx.ptr(FW_CFG_SELECTOR);
        self.port_out(fx, 2, selector, item);
    }

    /// The next byte of the selected item, zero-extended.
    fn fw_cfg_byte(&mut self, fx: &mut Fx) -> Value {
        if self.ck.db.target.arch() == Some(Arch::Aarch64) {
            let data = fx.ptr(FW_CFG_MMIO_DATA);
            return self.mmio_read(fx, 1, data);
        }
        let data = fx.ptr(FW_CFG_DATA);
        self.port_in(fx, 1, data)
    }

    /// A big-endian number of `width` bytes read from the configuration device.
    fn fw_cfg_number(&mut self, fx: &mut Fx, width: u64) -> Value {
        let mut value = fx.ptr(0);
        for _ in 0..width {
            let byte = self.fw_cfg_byte(fx);
            let shifted = fx.b.ins().ishl_imm_s(value, 8);
            value = fx.b.ins().bor(shifted, byte);
        }
        value
    }
}
