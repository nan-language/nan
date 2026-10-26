kind: write
area: kernel

In the package `ticker`, write `pub fn Ticker.tick(mut self, t: hw::IrqsDisabled) -> bool limits notrap, terminates, noalloc` and the interrupt fn `Ticker.on_irq` that `attach` installs. `tick` reads `status`; when it does not show `expired`, it returns false and does nothing else. Otherwise it acknowledges the expiry by writing `status` with only `expired` set, counts one tick in `ticks` (modulo 2^64), and, when `period` is not 0 and the new count is a multiple of it, posts the count to `wake` and returns true; it returns false otherwise. `on_irq` takes the ticker by `mut` and the interrupt context, runs `tick`, completes the interrupt, and returns `hw::IrqExit.Reschedule` when `tick` posted and `hw::IrqExit.Resume` otherwise; its stack limit is 1_024 bytes.
