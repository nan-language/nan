//! A kernel's clock: its counter, x86_64's time-stamp counter or AArch64's virtual counter, in
//! nanoseconds since the runtime started and never below what any CPU read before; the timers
//! of the clock, which each CPU's timer interrupt expires, its timer programmed one-shot for the
//! earlier of its next pacing tick and the earliest timer due; and the wall clock source a
//! machine root grant reads once.
//!
//! On x86_64 the time-stamp counter's rate is measured against the 10 ms the legacy PIT's channel
//! 2 counts down from `PIT_LATCH`, and each CPU measures its local APIC timer's rate against the
//! clock as it starts taking interrupts. The wall clock source is the CMOS clock on
//! `x86_64-bare`, the PL031 at `0x0901_0000` on `aarch64-bare` and the firmware's `GetTime` on the
//! UEFI targets, its time read as UTC.

use cranelift_codegen::ir::{InstBuilder, MemFlagsData, Value, condcodes::IntCC, types};

use super::super::Lowering;
use super::super::fx::{Fx, PTR};
use super::super::runtime::tasks::{self, CANCELED, EXPIRED, operation};
use super::super::runtime::{Routine, state, timers};
use super::super::supplied::boot::firmware;
use super::cpus::cpu;
use super::stubs::Stub;
use crate::workspace::{Arch, Target};

/// The nanoseconds between a CPU's pacing ticks.
const PACING: u64 = 10_000_000;

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

/// The PL031 of `aarch64-bare`, whose data register at its base counts the seconds since the
/// Unix epoch.
const PL031: u64 = 0x0901_0000;

/// The nanoseconds of a second.
const NS_PER_SECOND: u64 = 1_000_000_000;

impl Lowering<'_, '_> {
    /// Starts the clock in `Init`: its counter now is its base, and on x86_64 the time-stamp
    /// counter's rate is measured against the PIT.
    pub(super) fn start_clock(&mut self, fx: &mut Fx) {
        let state_address = self.state(fx);
        let (base, scale) = match self.machine_arch() {
            Arch::Amd64 => {
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
                (end, fx.b.ins().udiv(window, ticks))
            }
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

    /// `ClockNow() -> nanoseconds`: the counter's ticks since the base, as nanoseconds, raised to
    /// the latest reading of any CPU when they are fewer.
    pub fn define_clock_now(&mut self, fx: &mut Fx) {
        fx.entry();
        let count = match self.machine_arch() {
            Arch::Amd64 => self.call_stub(fx, Stub::ReadTsc, &[])[0],
            Arch::Aarch64 => self.read_sysreg(fx, super::aarch64::CNTVCT_EL0),
        };
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
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        self.program_timer(fx);
        self.call_routine(fx, Routine::Unlock, &[lock]);
    }

    /// Programs the running CPU's timer to fire at the earlier of its next pacing tick and the
    /// earliest timer due, at once when that has passed. The caller holds the timer lock.
    fn program_timer(&mut self, fx: &mut Fx) {
        let record = self.running_cpu(fx);
        let pace = fx.load(PTR, record, cpu::NEXT_PACE);
        let state_address = self.state(fx);
        let first = fx.load(PTR, state_address, state::TIMERS);
        let armed = fx.b.ins().icmp_imm_s(IntCC::NotEqual, first, 0);
        let target = fx.choose(
            armed,
            &[PTR],
            |fx| {
                let due = fx.load(PTR, first, operation::DUE);
                vec![fx.b.ins().umin(due, pace)]
            },
            |_| vec![pace],
        )[0];
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let ahead = fx.b.ins().icmp(IntCC::UnsignedGreaterThan, target, now);
        let delay = fx.b.ins().isub(target, now);
        let zero = fx.ptr(0);
        let delay = fx.b.ins().select(ahead, delay, zero);
        let rate = fx.load(PTR, record, cpu::TIMER_RATE);
        let ticks = fx.b.ins().imul(delay, rate);
        let per_ms = fx.ptr(timers::NS_PER_MS);
        let ticks = fx.b.ins().udiv(ticks, per_ms);
        let one = fx.ptr(1);
        let ticks = fx.b.ins().umax(ticks, one);
        let most = fx.ptr(0xFFFF_FFFF);
        let ticks = fx.b.ins().umin(ticks, most);
        match self.machine_arch() {
            Arch::Amd64 => self.x64_set_timer(fx, ticks),
            Arch::Aarch64 => self.aarch64_set_timer(fx, ticks),
        }
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
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        self.program_timer(fx);
        self.call_routine(fx, Routine::Unlock, &[lock]);
        pacing
    }

    /// Takes each timer due out of the timers, one at a time under the timer lock, and delivers
    /// its expiry outside it.
    fn expire_timers(&mut self, fx: &mut Fx) {
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

    /// Arms a timer of the clock due `ns` nanoseconds from now; when it is the earliest, the
    /// running CPU's timer is programmed again.
    pub fn kernel_arm(&mut self, fx: &mut Fx, op: Value, ns: Value) {
        let state_address = self.state(fx);
        let lock = fx.offset(state_address, state::TIMER_LOCK);
        self.call_routine(fx, Routine::Lock, &[lock]);
        let now = self.call_routine(fx, Routine::ClockNow, &[])[0];
        let due = timers::due_after(fx, now, ns);
        timers::arm(self, fx, op, due);
        let first = fx.load(PTR, state_address, state::TIMERS);
        let earliest = fx.b.ins().icmp(IntCC::Equal, first, op);
        fx.when(earliest, |fx| self.program_timer(fx));
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
    /// nanoseconds since the Unix epoch.
    pub fn wall_source(&mut self, fx: &mut Fx) -> (Value, Value) {
        match self.ck.db.target {
            Target::X64Bare => {
                let valid = fx.int(types::I8, 1);
                (valid, self.cmos_time(fx))
            }
            Target::Aarch64Bare => {
                let address = fx.ptr(PL031);
                let seconds = self.mmio_read(fx, 4, address);
                let valid = fx.int(types::I8, 1);
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
