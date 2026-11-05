//! A kernel's clock: its counter, on x86_64 the time-stamp counter, the HPET or the PM timer and
//! on AArch64 the virtual counter, in nanoseconds since the runtime started and never below what
//! any CPU read before; the timers of the clock, which each CPU's timer interrupt expires once
//! due, its timer programmed for the earliest of the end of a timer's window, the end of its
//! task's slice and its next pacing tick, which it has while it runs a task or the final build
//! binds the timer's vector; and the wall clock source a machine root grant reads once.
//!
//! On x86_64 the counter is the time-stamp counter where it is invariant, at the rate CPUID leaf
//! `0x15` reports, else measured over 10 ms of the HPET or else the PM timer the ACPI tables name,
//! else over the 10 ms the legacy PIT's channel 2 counts down from `PIT_LATCH`; elsewhere it is
//! that HPET or PM timer, its count extended to 64 bits from the latest any CPU read. Each CPU's
//! timer runs in TSC-deadline mode where it has that mode and the clock is the time-stamp
//! counter, and otherwise one-shot, its local APIC timer's rate measured against the clock as it
//! starts taking interrupts. The wall clock source is the CMOS clock on `x86_64-bare`, the PL031
//! the machine's discovery finds on `aarch64-bare` and the firmware's `GetTime` on the UEFI
//! targets, its time read as UTC.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tasks::{self, CANCELED, EXPIRED, operation};
use super::super::runtime::{Routine, state, timers};
use super::super::supplied::boot::firmware;
use super::acpi::{FADT_SIGNATURE, load_u32, load_u64};
use super::cpus::cpu;
use super::discovery::machine;
use super::stubs::Stub;
use crate::workspace::{Arch, Target};

/// The nanoseconds between a CPU's pacing ticks.
const PACING: u64 = 10_000_000;

/// The longest a CPU's timer waits, in nanoseconds, and the most ticks it counts down, which
/// AArch64's signed timer value holds.
const LONGEST_DELAY: u64 = 1_000_000_000;
const MOST_TICKS: u64 = 0x7FFF_FFFF;

/// The PIT's input clock, the count its channel 2 counts down while the time-stamp counter is
/// measured, and the nanoseconds that count takes; its ports: channel 2's data port, the mode
/// port and the system control port, whose bit 0 gates channel 2, bit 1 enables the speaker and
/// bit 5 reads channel 2's output; and the mode selecting channel 2, both bytes of its count,
/// its interrupt on terminal count and a binary count.
const PIT_HZ: u64 = 1_193_182;
const PIT_LATCH: u64 = 11_932;
const PIT_WINDOW: u64 = PIT_LATCH * 1_000_000_000 / PIT_HZ;
const PIT_CHANNEL_2: u64 = 0x42;
const PIT_MODE: u64 = 0x43;
const SYSTEM_CONTROL: u64 = 0x61;
const CHANNEL_2_ONE_SHOT: u64 = 0xB0;
const GATE: i64 = 0x01;
const SPEAKER: i64 = 0x02;
const CHANNEL_2_OUT: i64 = 0x20;

/// The clock's sources on x86_64: the time-stamp counter, the HPET and the PM timer.
const TSC_SOURCE: i64 = 0;
const HPET_SOURCE: i64 = 1;
const PM_TIMER_SOURCE: i64 = 2;

/// The CPUID leaves giving the highest extended leaf and, in bit 8 of EDX, an invariant
/// time-stamp counter; and those giving the highest basic leaf and the time-stamp counter's
/// ratio to the core crystal, as EBX over EAX, with the crystal's frequency in ECX.
const EXTENDED_LEAVES: u32 = 0x8000_0000;
const POWER_LEAF: u32 = 0x8000_0007;
const INVARIANT_TSC: i64 = 0x100;
const BASIC_LEAVES: u32 = 0;
const TSC_LEAF: u32 = 0x15;

/// The nanoseconds of the HPET or PM timer the time-stamp counter's rate is measured over.
const TSC_WINDOW: u64 = 10_000_000;

/// The signature of the ACPI table naming the HPET; the offset of the HPET's registers in it,
/// and those in the FADT of the PM timer's port and of the flags whose bit 8 marks a 32-bit PM
/// timer.
const HPET_SIGNATURE: i64 = 0x5445_5048;
const HPET_ADDRESS: u64 = 44;
const FADT_PM_TIMER: u64 = 76;
const FADT_FLAGS: u64 = 112;
const PM_TIMER_32: i64 = 0x100;

/// The HPET's registers past its capabilities at its base, whose bits 32 to 63 give its period in
/// femtoseconds and bit 13 a 64-bit counter: its configuration, whose bit 0 starts the counter,
/// and the counter; and the femtoseconds of a nanosecond.
const HPET_CONFIGURATION: u64 = 0x10;
const HPET_COUNTER: u64 = 0xF0;
const HPET_WIDE: i64 = 0x2000;
const HPET_ENABLE: i64 = 0x1;
const FS_PER_NS: u64 = 1_000_000;

/// The bytes of the HPET's registers.
const HPET_BYTES: u64 = 0x400;

/// The PM timer's rate.
const PM_TIMER_HZ: u64 = 3_579_545;

/// The CMOS clock's index and data ports, its registers of seconds, minutes, hours, day of the
/// month, month and year, and its status registers A, whose bit 7 is set during an update, and
/// B, whose bit 1 selects 24-hour hours and bit 2 binary values over BCD; and the bit of a
/// 12-hour hour set after noon.
const CMOS_INDEX: u64 = 0x70;
const CMOS_DATA: u64 = 0x71;
const CMOS_FIELDS: [u64; 6] = [0x00, 0x02, 0x04, 0x07, 0x08, 0x09];
const CMOS_STATUS_A: u64 = 0x0A;
const CMOS_STATUS_B: u64 = 0x0B;
const CMOS_UPDATING: i64 = 0x80;
const CMOS_24_HOUR: i64 = 0x02;
const CMOS_BINARY: i64 = 0x04;
const CMOS_AFTERNOON: i64 = 0x80;

/// The nanoseconds of a second.
const NS_PER_SECOND: u64 = 1_000_000_000;

impl Lowering<'_, '_> {
    /// Starts the clock in `Init`: its counter now is its base, and on x86_64 its source is
    /// chosen.
    pub(super) fn start_clock(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let (base, scale) = match self.machine_arch() {
            Arch::Amd64 => self.start_x64_clock(fx),
            Arch::Aarch64 => {
                let frequency = self.read_sysreg(fx, super::aarch64::CNTFRQ_EL0);
                let second = fx.ptr(NS_PER_SECOND << 32);
                let scale = fx.b.ins().udiv(second, frequency);
                (self.read_sysreg(fx, super::aarch64::CNTVCT_EL0), scale)
            }
        };
        fx.store(base, state_address, state::CLOCK_BASE);
        fx.store(scale, state_address, state::CLOCK_SCALE);
    }

    /// Chooses the x86_64 clock's source, recording it: the time-stamp counter where it is
    /// invariant, otherwise the HPET, else the PM timer, else the time-stamp counter. Gives the
    /// source's count now and its scale.
    fn start_x64_clock(&mut self, fx: &mut Fx) -> (Value, Value) {
        let registers = fx.slot(16, 4);
        let highest = self.cpuid(fx, registers, EXTENDED_LEAVES, 0);
        self.cpuid(fx, registers, POWER_LEAF, 0);
        let edx = fx.load(types::I32, registers, 12);
        let invariant = fx.b.ins().band_imm_s(edx, INVARIANT_TSC);
        let invariant = fx.b.ins().icmp_imm_s(IntCC::NotEqual, invariant, 0);
        let leaves = fx.b.ins().icmp_imm_s(
            IntCC::UnsignedGreaterThanOrEqual,
            highest,
            POWER_LEAF as i64,
        );
        let invariant = fx.b.ins().band(invariant, leaves);
        let platform = self.counter_source(fx);
        let counted =
            fx.b.ins()
                .icmp_imm_s(IntCC::NotEqual, platform[0], TSC_SOURCE);
        let uncounted = fx.b.ins().bxor_imm_s(counted, 1);
        let tsc = fx.b.ins().bor(invariant, uncounted);
        let tsc_source = fx.int(PTR, TSC_SOURCE);
        let source = fx.b.ins().select(tsc, tsc_source, platform[0]);
        let state_address = self.state(fx);
        fx.store(source, state_address, state::CLOCK_SOURCE);
        fx.store(platform[1], state_address, state::CLOCK_COUNTER);
        fx.store(platform[2], state_address, state::CLOCK_MASK);
        let started = fx.choose_in(
            self,
            tsc,
            &[PTR, PTR],
            |lo, fx| lo.tsc_rate(fx, counted, &platform),
            |lo, fx| {
                let count = lo.counter_reading(fx, platform[0], platform[1]);
                let state_address = lo.state(fx);
                fx.store(count, state_address, state::CLOCK_TICKS);
                vec![count, platform[3]]
            },
        );
        (started[0], started[1])
    }

    /// The time-stamp counter now and its scale: from the rate CPUID leaf `0x15` reports where
    /// it gives both the ratio and the crystal's frequency, else measured against `platform`,
    /// the source, counter, mask and scale of the HPET or PM timer, when `counted` holds, and
    /// against the PIT otherwise.
    fn tsc_rate(&mut self, fx: &mut Fx, counted: Value, platform: &[Value]) -> Vec<Value> {
        let registers = fx.slot(16, 4);
        let highest = self.cpuid(fx, registers, BASIC_LEAVES, 0);
        let denominator = self.cpuid(fx, registers, TSC_LEAF, 0);
        let ratio = fx.load(types::I32, registers, 4);
        let ratio = fx.b.ins().uextend(PTR, ratio);
        let crystal = fx.load(types::I32, registers, 8);
        let crystal = fx.b.ins().uextend(PTR, crystal);
        let listed =
            fx.b.ins()
                .icmp_imm_s(IntCC::UnsignedGreaterThanOrEqual, highest, TSC_LEAF as i64);
        let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, ratio, 0);
        let reported = fx.b.ins().band(listed, given);
        let given = fx.b.ins().icmp_imm_s(IntCC::NotEqual, crystal, 0);
        let reported = fx.b.ins().band(reported, given);
        fx.choose_in(
            self,
            reported,
            &[PTR, PTR],
            |lo, fx| {
                let hz = fx.b.ins().imul(crystal, ratio);
                let one = fx.ptr(1);
                let denominator = fx.b.ins().umax(denominator, one);
                let hz = fx.b.ins().udiv(hz, denominator);
                let hz = fx.b.ins().umax(hz, one);
                let second = fx.ptr(NS_PER_SECOND << 32);
                let scale = fx.b.ins().udiv(second, hz);
                let now = lo.call_stub(fx, Stub::ReadTsc, &[])[0];
                vec![now, scale]
            },
            |lo, fx| {
                fx.choose_in(
                    lo,
                    counted,
                    &[PTR, PTR],
                    |lo, fx| lo.counter_tsc_rate(fx, platform),
                    |lo, fx| lo.pit_tsc_rate(fx),
                )
            },
        )
    }

    /// The time-stamp counter at the end of `TSC_WINDOW` of a platform counter, given as its
    /// source, counter, mask and scale, and its scale over that window.
    fn counter_tsc_rate(&mut self, fx: &mut Fx, platform: &[Value]) -> Vec<Value> {
        let (source, counter, mask, scale) = (platform[0], platform[1], platform[2], platform[3]);
        let window = fx.ptr(TSC_WINDOW << 32);
        let window = fx.b.ins().udiv(window, scale);
        let first = self.counter_reading(fx, source, counter);
        let start = self.call_stub(fx, Stub::ReadTsc, &[])[0];
        let zero = fx.ptr(0);
        let passed = fx.repeat(&[zero], |fx, _| {
            let reading = self.counter_reading(fx, source, counter);
            let passed = fx.b.ins().isub(reading, first);
            let passed = fx.b.ins().band(passed, mask);
            let short = fx.b.ins().icmp(IntCC::UnsignedLessThan, passed, window);
            (short, vec![passed])
        })[0];
        let end = self.call_stub(fx, Stub::ReadTsc, &[])[0];
        let ticks = fx.b.ins().isub(end, start);
        let one = fx.ptr(1);
        let ticks = fx.b.ins().umax(ticks, one);
        let elapsed = fx.b.ins().imul(passed, scale);
        vec![end, fx.b.ins().udiv(elapsed, ticks)]
    }

    /// The HPET the ACPI `HPET` table names, its counter started, or else the PM timer the
    /// FADT names, or else the time-stamp counter: the source, its counter's address or port,
    /// the mask of the bits it counts and its scale.
    fn counter_source(&mut self, fx: &mut Fx) -> Vec<Value> {
        let rsdp = self.rsdp(fx);
        let hpet = self.acpi_table(fx, rsdp, HPET_SIGNATURE);
        let found = fx.b.ins().icmp_imm_s(IntCC::NotEqual, hpet, 0);
        fx.choose_in(
            self,
            found,
            &[PTR, PTR, PTR, PTR],
            |lo, fx| {
                let base = load_u64(fx, hpet, HPET_ADDRESS);
                let capabilities = lo.mmio_read(fx, 8, base);
                let at = fx.b.ins().iadd_imm_s(base, HPET_CONFIGURATION as i64);
                let configuration = lo.mmio_read(fx, 8, at);
                let enabled = fx.b.ins().bor_imm_s(configuration, HPET_ENABLE);
                lo.mmio_write(fx, 8, at, enabled);
                let period = fx.b.ins().ushr_imm_s(capabilities, 32);
                let period = fx.b.ins().ishl_imm_s(period, 32);
                let femtoseconds = fx.ptr(FS_PER_NS);
                let scale = fx.b.ins().udiv(period, femtoseconds);
                let wide = fx.b.ins().band_imm_s(capabilities, HPET_WIDE);
                let wide = fx.b.ins().icmp_imm_s(IntCC::NotEqual, wide, 0);
                let all = fx.int(PTR, -1);
                let low = fx.ptr(0xFFFF_FFFF);
                let mask = fx.b.ins().select(wide, all, low);
                let counter = fx.b.ins().iadd_imm_s(base, HPET_COUNTER as i64);
                let source = fx.int(PTR, HPET_SOURCE);
                vec![source, counter, mask, scale]
            },
            |lo, fx| {
                let fadt = lo.acpi_table(fx, rsdp, FADT_SIGNATURE);
                let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, fadt, 0);
                let timer = fx.choose(
                    present,
                    &[PTR, PTR],
                    |fx| {
                        let port = load_u32(fx, fadt, FADT_PM_TIMER);
                        let flags = load_u32(fx, fadt, FADT_FLAGS);
                        vec![port, flags]
                    },
                    |fx| {
                        let zero = fx.ptr(0);
                        vec![zero, zero]
                    },
                );
                let timed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, timer[0], 0);
                let pm_timer = fx.int(PTR, PM_TIMER_SOURCE);
                let tsc = fx.int(PTR, TSC_SOURCE);
                let source = fx.b.ins().select(timed, pm_timer, tsc);
                let wide = fx.b.ins().band_imm_s(timer[1], PM_TIMER_32);
                let wide = fx.b.ins().icmp_imm_s(IntCC::NotEqual, wide, 0);
                let all = fx.ptr(0xFFFF_FFFF);
                let low = fx.ptr(0xFF_FFFF);
                let mask = fx.b.ins().select(wide, all, low);
                let scale = fx.ptr((NS_PER_SECOND << 32) / PM_TIMER_HZ);
                vec![source, timer[0], mask, scale]
            },
        )
    }

    /// The time-stamp counter at the end of the 10 ms the PIT's channel 2 counts down, and its
    /// scale over those 10 ms.
    fn pit_tsc_rate(&mut self, fx: &mut Fx) -> Vec<Value> {
        let port = fx.ptr(SYSTEM_CONTROL);
        let control = self.port_in(fx, 1, port);
        let control = fx.b.ins().band_imm_s(control, !SPEAKER);
        let control = fx.b.ins().bor_imm_s(control, GATE);
        self.port_out(fx, 1, port, control);
        let port = fx.ptr(PIT_MODE);
        let mode = fx.ptr(CHANNEL_2_ONE_SHOT);
        self.port_out(fx, 1, port, mode);
        for byte in [PIT_LATCH & 0xFF, PIT_LATCH >> 8] {
            let port = fx.ptr(PIT_CHANNEL_2);
            let byte = fx.ptr(byte);
            self.port_out(fx, 1, port, byte);
        }
        let start = self.call_stub(fx, Stub::ReadTsc, &[])[0];
        fx.repeat(&[], |fx, _| {
            let port = fx.ptr(SYSTEM_CONTROL);
            let control = self.port_in(fx, 1, port);
            let out = fx.b.ins().band_imm_s(control, CHANNEL_2_OUT);
            (fx.b.ins().icmp_imm_s(IntCC::Equal, out, 0), Vec::new())
        });
        let end = self.call_stub(fx, Stub::ReadTsc, &[])[0];
        let ticks = fx.b.ins().isub(end, start);
        let one = fx.ptr(1);
        let ticks = fx.b.ins().umax(ticks, one);
        let window = fx.ptr(PIT_WINDOW << 32);
        vec![end, fx.b.ins().udiv(window, ticks)]
    }

    /// A reading of the HPET's counter or the PM timer's, as `source` is one or the other.
    fn counter_reading(&mut self, fx: &mut Fx, source: Value, counter: Value) -> Value {
        let hpet = fx.b.ins().icmp_imm_s(IntCC::Equal, source, HPET_SOURCE);
        fx.choose_in(
            self,
            hpet,
            &[PTR],
            |lo, fx| vec![lo.mmio_read(fx, 8, counter)],
            |lo, fx| vec![lo.port_in(fx, 4, counter)],
        )[0]
    }

    /// `ClockNow() -> nanoseconds`: the counter's ticks since the base, as nanoseconds, raised to
    /// the latest reading of any CPU when they are fewer.
    pub fn define_clock_now(&mut self, fx: &mut Fx) {
        fx.entry();
        let count = self.clock_count(fx);
        let state_address = self.state(fx);
        let base = fx.load(PTR, state_address, state::CLOCK_BASE);
        let scale = fx.load(PTR, state_address, state::CLOCK_SCALE);
        let before = fx.b.ins().icmp(IntCC::UnsignedLessThan, count, base);
        let ticks = fx.b.ins().isub(count, base);
        let zero = fx.ptr(0);
        let ticks = fx.b.ins().select(before, zero, ticks);
        let low = fx.b.ins().imul(ticks, scale);
        let high = fx.b.ins().umulhi(ticks, scale);
        let low = fx.b.ins().ushr_imm_s(low, 32);
        let high = fx.b.ins().ishl_imm_s(high, 32);
        let read = fx.b.ins().bor(high, low);
        let last = fx.offset(state_address, state::CLOCK_LAST);
        let now = fx.repeat(&[zero], |fx, _| {
            let latest = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), last);
            let behind =
                fx.b.ins()
                    .icmp(IntCC::UnsignedLessThanOrEqual, read, latest);
            let result = fx.choose(
                behind,
                &[types::I8, PTR],
                |fx| vec![fx.int(types::I8, 0), latest],
                |fx| {
                    let seen =
                        fx.b.ins()
                            .atomic_cas(MemFlagsData::trusted(), last, latest, read);
                    let raced = fx.b.ins().icmp(IntCC::NotEqual, seen, latest);
                    vec![raced, read]
                },
            );
            (result[0], vec![result[1]])
        })[0];
        fx.b.ins().return_(&[now]);
    }

    /// Whether the x86_64 clock's source is the time-stamp counter.
    pub(super) fn tsc_clocked(&mut self, fx: &mut Fx) -> Value {
        let state_address = self.state(fx);
        let source = fx.load(PTR, state_address, state::CLOCK_SOURCE);
        fx.b.ins().icmp_imm_s(IntCC::Equal, source, TSC_SOURCE)
    }

    /// The first address and last address plus one of the registers the clock reads through
    /// memory: the HPET's where it is the x86_64 clock's source, none otherwise.
    pub fn clock_registers_extent(&mut self, fx: &mut Fx) -> (Value, Value) {
        let zero = fx.ptr(0);
        if self.machine_arch() == Arch::Aarch64 {
            return (zero, zero);
        }
        let state_address = self.state(fx);
        let source = fx.load(PTR, state_address, state::CLOCK_SOURCE);
        let counter = fx.load(PTR, state_address, state::CLOCK_COUNTER);
        let hpet = fx.b.ins().icmp_imm_s(IntCC::Equal, source, HPET_SOURCE);
        let base = fx.b.ins().iadd_imm_s(counter, -(HPET_COUNTER as i64));
        let end = fx.offset(base, HPET_BYTES);
        let start = fx.b.ins().select(hpet, base, zero);
        let end = fx.b.ins().select(hpet, end, zero);
        (start, end)
    }

    /// The clock's counter: on x86_64 the time-stamp counter, or the HPET's or PM timer's count
    /// extended; on AArch64 the virtual counter.
    fn clock_count(&mut self, fx: &mut Fx) -> Value {
        if self.machine_arch() == Arch::Aarch64 {
            return self.read_sysreg(fx, super::aarch64::CNTVCT_EL0);
        }
        let state_address = self.state(fx);
        let source = fx.load(PTR, state_address, state::CLOCK_SOURCE);
        let tsc = fx.b.ins().icmp_imm_s(IntCC::Equal, source, TSC_SOURCE);
        fx.choose_in(
            self,
            tsc,
            &[PTR],
            |lo, fx| lo.call_stub(fx, Stub::ReadTsc, &[]),
            |lo, fx| vec![lo.extended_count(fx, source)],
        )[0]
    }

    /// The HPET's or PM timer's count extended to 64 bits from the latest count any CPU read,
    /// which a reading less than half the counter's range behind it gives instead.
    fn extended_count(&mut self, fx: &mut Fx, source: Value) -> Value {
        let state_address = self.state(fx);
        let counter = fx.load(PTR, state_address, state::CLOCK_COUNTER);
        let mask = fx.load(PTR, state_address, state::CLOCK_MASK);
        let half = fx.b.ins().ushr_imm_s(mask, 1);
        let ticks = fx.offset(state_address, state::CLOCK_TICKS);
        let zero = fx.ptr(0);
        fx.repeat(&[zero], |fx, _| {
            let latest = fx.b.ins().atomic_load(PTR, MemFlagsData::trusted(), ticks);
            let reading = self.counter_reading(fx, source, counter);
            let passed = fx.b.ins().isub(reading, latest);
            let passed = fx.b.ins().band(passed, mask);
            let behind = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, passed, half);
            let result = fx.choose(
                behind,
                &[types::I8, PTR],
                |fx| vec![fx.int(types::I8, 0), latest],
                |fx| {
                    let count = fx.b.ins().iadd(latest, passed);
                    let seen =
                        fx.b.ins()
                            .atomic_cas(MemFlagsData::trusted(), ticks, latest, count);
                    let raced = fx.b.ins().icmp(IntCC::NotEqual, seen, latest);
                    vec![raced, count]
                },
            );
            (result[0], vec![result[1]])
        })[0]
    }

    /// Starts the running CPU's timer as it starts taking interrupts: its rate measured on
    /// x86_64, its first pacing tick a period from now.
    pub(super) fn start_timer(&mut self, fx: &mut Fx) {
        let rate = match self.machine_arch() {
            Arch::Amd64 => self.x64_timer_rate(fx),
            Arch::Aarch64 => self.aarch64_timer_rate(fx),
        };
        let record = self.running_cpu(fx);
        fx.store(rate, record, cpu::TIMER_RATE);
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let pace = fx.b.ins().iadd_imm_s(now, PACING as i64);
        fx.store(pace, record, cpu::NEXT_PACE);
        self.reprogram_timer(fx);
    }

    /// Programs the running CPU's timer again, under the timer lock.
    pub(super) fn reprogram_timer(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        self.program_timer(fx);
        self.call_routine(fx, Routine::Unlock, &[lock]);
    }

    /// Programs the running CPU's timer to fire at the earliest of the end of a timer's window,
    /// the end of its task's slice while that is ahead and, while it runs a task or the final
    /// build binds the timer's vector, its next pacing tick; at once when that has passed. The
    /// caller holds the timer lock.
    fn program_timer(&mut self, fx: &mut Fx) {
        let record = self.running_cpu(fx);
        let latest = fx.int(PTR, -1);
        let pace = fx.load(PTR, record, cpu::NEXT_PACE);
        let pace = match self.statically_bound().contains(&self.vectors().timer) {
            true => pace,
            false => {
                let thread = fx.load(PTR, record, cpu::THREAD);
                let running = fx.load(PTR, thread, tasks::thread::CURRENT);
                let running = fx.b.ins().icmp_imm_s(IntCC::NotEqual, running, 0);
                fx.b.ins().select(running, pace, latest)
            }
        };
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let end = fx.load(PTR, record, cpu::SLICE_END);
        let sliced = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, end, now);
        let end = fx.b.ins().select(sliced, end, latest);
        let target = fx.b.ins().umin(pace, end);
        let target = self.windows_end(fx, target);
        let ahead = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, target, now);
        let delay = fx.b.ins().isub(target, now);
        let zero = fx.ptr(0);
        let delay = fx.b.ins().select(ahead, delay, zero);
        let longest = fx.ptr(LONGEST_DELAY);
        let delay = fx.b.ins().umin(delay, longest);
        let rate = fx.load(PTR, record, cpu::TIMER_RATE);
        let ticks = fx.b.ins().imul(delay, rate);
        let per_ms = fx.ptr(timers::NS_PER_MS);
        let ticks = fx.b.ins().udiv(ticks, per_ms);
        let one = fx.ptr(1);
        let ticks = fx.b.ins().umax(ticks, one);
        let most = fx.ptr(MOST_TICKS);
        let ticks = fx.b.ins().umin(ticks, most);
        match self.machine_arch() {
            Arch::Amd64 => self.x64_set_timer(fx, ticks),
            Arch::Aarch64 => self.aarch64_set_timer(fx, ticks),
        }
    }

    /// The earliest end of a timer's window, or `bound` when it is earlier. The timers are in the
    /// order they are due and a window ends no sooner than its timer is due, so the walk stops
    /// at the first timer due after the earliest end so far. The caller holds the timer lock.
    pub(super) fn windows_end(&mut self, fx: &mut Fx, bound: Value) -> Value {
        let state_address = self.state(fx);
        let first = fx.load(PTR, state_address, state::TIMERS);
        fx.repeat(&[first, bound], |fx, walk| {
            let (current, end) = (walk[0], walk[1]);
            let present = fx.b.ins().icmp_imm_s(IntCC::NotEqual, current, 0);
            let next = fx.choose(
                present,
                &[PTR, PTR, types::I8],
                |fx| {
                    let due = fx.load(PTR, current, operation::DUE);
                    let within = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, due, end);
                    let window = fx.load(PTR, current, operation::WINDOW_END);
                    vec![
                        fx.load(PTR, current, operation::NEXT),
                        fx.b.ins().umin(end, window),
                        within,
                    ]
                },
                |fx| vec![current, end, fx.int(types::I8, 0)],
            );
            (next[2], vec![next[0], next[1]])
        })[1]
    }

    /// What the running CPU's timer interrupt does before any interrupt fn runs: the timers due
    /// expire, a pacing tick that has come moves the next a period on, and the timer is
    /// programmed again. Gives whether the interrupt is a pacing tick.
    pub(super) fn timer_interrupt(&mut self, fx: &mut Fx) -> Value {
        self.expire_timers(fx);
        let record = self.running_cpu(fx);
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let pace = fx.load(PTR, record, cpu::NEXT_PACE);
        let pacing =
            fx.b.ins()
                .icmp(IntCC::UnsignedGreaterThanOrEqual, now, pace);
        fx.when(pacing, |fx| {
            let next = fx.b.ins().iadd_imm_s(pace, PACING as i64);
            let late = fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, next, now);
            let fresh = fx.b.ins().iadd_imm_s(now, PACING as i64);
            let next = fx.b.ins().select(late, fresh, next);
            fx.store(next, record, cpu::NEXT_PACE);
        });
        self.reprogram_timer(fx);
        pacing
    }

    /// Takes each timer due out of the timers, one at a time under the timer lock, and delivers
    /// its expiry outside it.
    pub(crate) fn expire_timers(&mut self, fx: &mut Fx) {
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        fx.repeat(&[], |fx, _| {
            let state_address = self.state(fx);
            let lock = fx.offset(state_address, state::TIMER_LOCK);
            self.call_routine(fx, Routine::Lock, &[lock]);
            let first = fx.load(PTR, state_address, state::TIMERS);
            let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
            let due = fx.choose(
                armed,
                &[types::I8],
                |fx| {
                    let due = fx.load(PTR, first, operation::DUE);
                    vec![fx.b.ins().icmp(IntCC::UnsignedLessThanOrEqual, due, now)]
                },
                |fx| vec![fx.int(types::I8, 0)],
            )[0];
            fx.when(due, |fx| {
                timers::disarm(self, fx, first);
                claim(fx, first);
            });
            self.call_routine(fx, Routine::Unlock, &[lock]);
            fx.when(due, |fx| {
                let expired = fx.int(types::I32, EXPIRED);
                tasks::deliver(self, fx, first, expired);
            });
            (due, Vec::new())
        });
    }

    /// Arms a timer of the clock due `ns` nanoseconds from now through `TimerArm`, on whose stack
    /// the policy's slack hook runs.
    pub fn kernel_arm(&mut self, fx: &mut Fx, op: Value, ns: Value) {
        self.call_routine(fx, Routine::TimerArm, &[op, ns]);
    }

    /// Arms a timer of the clock due `ns` nanoseconds from now. On a kernel its window ends
    /// after the slack the kernel's policy gives it, and when that window ends first the running
    /// CPU's timer is programmed again; an isolated space's runtime waits for it as it idles.
    pub fn arm_clock(&mut self, fx: &mut Fx, op: Value, ns: Value) {
        let slack = (!self.user()).then(|| self.timer_slack(fx, ns));
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let due = timers::due_after(fx, now, ns);
        timers::arm(self, fx, op, due);
        if let Some(slack) = slack {
            let window = timers::due_after(fx, due, slack);
            fx.store(window, op, operation::WINDOW_END);
            let all = fx.int(PTR, -1);
            let end = self.windows_end(fx, all);
            let earliest = fx.b.ins().icmp(IntCC::Equal, end, window);
            fx.when(earliest, |fx| self.program_timer(fx));
        }
        self.call_routine(fx, Routine::Unlock, &[lock]);
    }

    /// Cancels a timer of the clock that is armed and has not expired: it leaves the timers and
    /// completes cancelled.
    pub fn kernel_cancel(&mut self, fx: &mut Fx, op: Value) {
        let claimed = self.kernel_claim(fx, op);
        fx.when(claimed, |fx| self.kernel_cancelled(fx, op));
    }

    /// Takes a timer of the clock that is armed and has not expired out of the timers, giving
    /// whether it did so; its cancelled completion is then the caller's to deliver.
    pub fn kernel_claim(&mut self, fx: &mut Fx, op: Value) -> Value {
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        let done = fx.load(PTR, op, operation::DONE);
        let pending = fx.b.ins().icmp_imm_s(IntCC::Equal, done, 0);
        let order = fx.load(PTR, op, operation::ORDER);
        let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, order, 0);
        let in_flight = fx.b.ins().band(pending, armed);
        fx.when(in_flight, |fx| {
            timers::disarm(self, fx, op);
            claim(fx, op);
        });
        self.call_routine(fx, Routine::Unlock, &[lock]);
        in_flight
    }

    /// Delivers the cancelled completion of a timer the caller claimed.
    pub fn kernel_cancelled(&mut self, fx: &mut Fx, op: Value) {
        let cancelled = fx.int(types::I32, CANCELED);
        tasks::deliver(self, fx, op, cancelled);
    }

    /// Reads the target's wall clock source: whether it gave a time, and that time in
    /// nanoseconds since the Unix epoch. The PL031's data register, at its base, counts the
    /// seconds since the epoch; an `aarch64-bare` machine without one gives no time.
    pub fn wall_source(&mut self, fx: &mut Fx) -> (Value, Value) {
        match self.ck.db.target {
            Target::X64Bare => {
                let valid = fx.int(types::I8, 1);
                (valid, self.cmos_time(fx))
            }
            Target::Aarch64Bare => {
                let record = self.machine_record(fx);
                let rtc = fx.load(PTR, record, machine::RTC);
                let valid = fx.b.ins().icmp_imm_s(IntCC::NotEqual, rtc, 0);
                let seconds = fx.choose_in(
                    self,
                    valid,
                    &[PTR],
                    |lo, fx| vec![lo.mmio_read(fx, 4, rtc)],
                    |_, fx| vec![fx.ptr(0)],
                )[0];
                (valid, fx.b.ins().imul_imm_s(seconds, NS_PER_SECOND as i64))
            }
            _ => {
                let (valid, fields, nanoseconds) = firmware::time(self, fx);
                let seconds = civil_seconds(fx, fields);
                let ns = fx.b.ins().imul_imm_s(seconds, NS_PER_SECOND as i64);
                (valid, fx.b.ins().iadd(ns, nanoseconds))
            }
        }
    }

    /// The CMOS clock's time in nanoseconds since the Unix epoch, read once no update runs, its
    /// year in the century from 2000.
    fn cmos_time(&mut self, fx: &mut Fx) -> Value {
        fx.repeat(&[], |fx, _| {
            let status = self.cmos(fx, CMOS_STATUS_A);
            let updating = fx.b.ins().band_imm_s(status, CMOS_UPDATING);
            (
                fx.b.ins().icmp_imm_s(IntCC::NotEqual, updating, 0),
                Vec::new(),
            )
        });
        let raw: Vec<Value> = CMOS_FIELDS.iter().map(|at| self.cmos(fx, *at)).collect();
        let format = self.cmos(fx, CMOS_STATUS_B);
        let binary = fx.b.ins().band_imm_s(format, CMOS_BINARY);
        let binary = fx.b.ins().icmp_imm_s(IntCC::NotEqual, binary, 0);
        let afternoon = fx.b.ins().band_imm_s(raw[2], CMOS_AFTERNOON);
        let afternoon = fx.b.ins().icmp_imm_s(IntCC::NotEqual, afternoon, 0);
        let mut fields: Vec<Value> = raw
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = match index {
                    2 => fx.b.ins().band_imm_s(*value, !CMOS_AFTERNOON),
                    _ => *value,
                };
                let low = fx.b.ins().band_imm_s(value, 0x0F);
                let high = fx.b.ins().ushr_imm_s(value, 4);
                let tens = fx.b.ins().imul_imm_s(high, 10);
                let decimal = fx.b.ins().iadd(low, tens);
                fx.b.ins().select(binary, value, decimal)
            })
            .collect();
        let hours = fx.b.ins().band_imm_s(format, CMOS_24_HOUR);
        let twelve = fx.b.ins().icmp_imm_s(IntCC::Equal, hours, 0);
        let hour = fx.ptr(12);
        let hour = fx.b.ins().urem(fields[2], hour);
        let later = fx.b.ins().iadd_imm_s(hour, 12);
        let hour = fx.b.ins().select(afternoon, later, hour);
        fields[2] = fx.b.ins().select(twelve, hour, fields[2]);
        fields[5] = fx.b.ins().iadd_imm_s(fields[5], 2000);
        let civil = [
            fields[5], fields[4], fields[3], fields[2], fields[1], fields[0],
        ];
        let seconds = civil_seconds(fx, civil);
        fx.b.ins().imul_imm_s(seconds, NS_PER_SECOND as i64)
    }

    /// A register of the CMOS clock.
    fn cmos(&mut self, fx: &mut Fx, register: u64) -> Value {
        let index = fx.ptr(CMOS_INDEX);
        let register = fx.ptr(register);
        self.port_out(fx, 1, index, register);
        let data = fx.ptr(CMOS_DATA);
        self.port_in(fx, 1, data)
    }
}

/// Marks a timer taken out of the timers as completed, so no other CPU delivers it.
fn claim(fx: &mut Fx, op: Value) {
    let completed = fx.ptr(operation::COMPLETED as u64);
    fx.store(completed, op, operation::DONE);
}

/// The seconds since the Unix epoch of a time from 2000 on: its year, month, day, hour, minute
/// and second, counting days as the proleptic Gregorian calendar does.
fn civil_seconds(fx: &mut Fx, [year, month, day, hour, minute, second]: [Value; 6]) -> Value {
    let early =
        fx.b.ins()
            .icmp_imm_s(IntCC::UnsignedLessThanOrEqual, month, 2);
    let borrow = fx.b.ins().uextend(PTR, early);
    let year = fx.b.ins().isub(year, borrow);
    let era = fx.ptr(400);
    let era = fx.b.ins().udiv(year, era);
    let start = fx.b.ins().imul_imm_s(era, 400);
    let of_era = fx.b.ins().isub(year, start);
    let shifted = fx.b.ins().iadd_imm_s(month, -3);
    let wrapped = fx.b.ins().iadd_imm_s(month, 9);
    let shifted = fx.b.ins().select(early, wrapped, shifted);
    let days = fx.b.ins().imul_imm_s(shifted, 153);
    let days = fx.b.ins().iadd_imm_s(days, 2);
    let five = fx.ptr(5);
    let days = fx.b.ins().udiv(days, five);
    let days = fx.b.ins().iadd(days, day);
    let of_year = fx.b.ins().iadd_imm_s(days, -1);
    let years = fx.b.ins().imul_imm_s(of_era, 365);
    let four = fx.ptr(4);
    let leap = fx.b.ins().udiv(of_era, four);
    let hundred = fx.ptr(100);
    let centuries = fx.b.ins().udiv(of_era, hundred);
    let days = fx.b.ins().iadd(years, leap);
    let days = fx.b.ins().isub(days, centuries);
    let days = fx.b.ins().iadd(days, of_year);
    let eras = fx.b.ins().imul_imm_s(era, 146_097);
    let days = fx.b.ins().iadd(eras, days);
    let days = fx.b.ins().iadd_imm_s(days, -719_468);
    let seconds = fx.b.ins().imul_imm_s(days, 86_400);
    let hours = fx.b.ins().imul_imm_s(hour, 3_600);
    let minutes = fx.b.ins().imul_imm_s(minute, 60);
    let seconds = fx.b.ins().iadd(seconds, hours);
    let seconds = fx.b.ins().iadd(seconds, minutes);
    fx.b.ins().iadd(seconds, second)
}
