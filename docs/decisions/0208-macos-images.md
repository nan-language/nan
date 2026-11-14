# 0208. macOS images

Status: accepted

## Context

[0206](0206-the-macos-targets.md) made the macOS targets checkable but built no image of them. A macOS image needs an image format the toolchain can write, calls into the system through `libSystem`, the one interface macOS keeps stable, and a runtime for executor threads, waits, termination requests, stack overflows and stops without io_uring or `signalfd`. AArch64 macOS runs no executable without a code signature, and the toolchain also runs on Linux and Windows machines, where no Mach-O linker is at hand.

## Decision

- **The image writer builds Mach-O executables** from the freestanding object lowering emits, as it builds PE images, so that macOS images build on every machine, byte for byte alike. They are position-independent, as macOS slides every executable: code reaches each imported function through a global offset table the writer lays out in a segment dyld makes read-only once bound, and dyld rebases each absolute address of data. Segments are aligned to AArch64 macOS's 16 KiB pages, and the writer signs each image ad hoc with SHA-256 page hashes.
- **macOS 15 or later**, whose `O_RESOLVE_BENEATH` keeps a path's resolution beneath its directory as `RESOLVE_BENEATH` does on Linux.
- **One kqueue serves every executor thread.** A waiting primitive waits there for its descriptor to be ready, through a one-shot event specific to its operation record, then performs its operation on the descriptor, which does not hold its thread; deleting the event cancels the wait exactly when no thread has taken it. Each executor thread has a user event of the kqueue that wakes it. Timers are the runtime's own list, as on Windows, which a thread waiting idle times out on.
- **Faults, stops and termination requests go through signals, as on Linux.** A stack overflow arrives as `SIGBUS` or `SIGSEGV` on the thread's signal stack, a stop as `SIGURG` sent to the thread; termination requests, which the process ignores, reach a thread of their own through a kqueue's signal events.
- **Linux and macOS share their POSIX routines**: records over the C library's heap, mapped stacks and their guards, signal stacks and handlers, and the `main` seam, each system giving its C library's values and layouts.
- **`macos_platform` implements the sys capabilities** over `libSystem` through foreign fns of `"darwin"`. No foreign fn is variadic, and Apple's AArch64 convention passes variadic arguments on the stack, so it declares `openat` and `fcntl` with their variadic argument both in its place and as a ninth argument, which AArch64 passes on the stack and x86_64 leaves unread.
- **A macOS machine of the target's architecture runs macOS images and tests**, as their system's processes.
- The macOS targets and `"darwin"` stay as [0206](0206-the-macos-targets.md) decided them.

Alternatives rejected:

- **Linking with Apple's linker:** macOS images would build only where Apple's SDK is installed.
- **A kqueue per executor thread, as io_uring is on Linux:** a ready descriptor would wait for the thread that registered it, however busy, while an idle thread could serve it.
- **The kqueue's own timers:** a cancelled timer completes at once only in a list the runtime keeps under its lock, which the Windows runtime already keeps.
- **A variadic form of foreign fns:** a language change for two functions of one platform package.

## Consequences

- macOS programs build on every machine and run only on macOS of the target's architecture; a test run on a macOS target needs a Mac.
- Every file operation, and a read or write of a standard stream that is a file or a device, holds its executor thread.
- A process ignores `SIGINT`, `SIGTERM` and `SIGHUP`, which the processes it starts inherit unless it resets them.
