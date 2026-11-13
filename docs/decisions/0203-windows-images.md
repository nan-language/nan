# 0203. Windows images

Status: accepted

## Context

[0202](0202-the-windows-targets.md) made the Windows targets checkable but built no image of them. A Windows image needs an image format the toolchain can write, calls into the system without a C library, and a runtime for executor threads, waits, termination requests, stack overflows and stops without io_uring or signals; and the toolchain runs on Linux machines, where its images and tests must still run.

## Decision

- **The image writer builds PE32+ console images** from the freestanding object lowering emits, as it builds UEFI images, so that no Windows linker or import library is needed and the image is reproducible. The image imports by name the functions its support routines call, from `kernel32.dll` and `bcryptprimitives.dll`, and each foreign fn it reaches from `kernel32.dll`; each call goes through the import address table's word.
- **One completion port serves every executor thread.** Sockets complete overlapped requests there; a standard stream, which may be a console or a synchronous pipe, runs each read and write on a thread of its own, which posts the completion; files are read and written synchronously. Timers are the runtime's own list, as on a kernel, which a thread waiting on the port times out on.
- **Faults and stops go through the system.** A vectored exception handler turns a stack overflow in a task stack's guard region into `trap.stack-overflow` on the thread's trap stack. The thread environment block records the bounds of the stack each task runs on, so that the system's guard pages and stack probes hold on task stacks. Settlement stops a running task by suspending its thread and redirecting its saved registers, as the Linux stop signal's handler does. The console's control handler passes termination requests to the port.
- **`windows_platform` implements the sys capabilities.** It reaches Winsock through `LoadLibraryExW` and `GetProcAddress` and calls it through a supplied `call`, since a foreign fn names no library.
- **Wine runs Windows images and tests**, as QEMU runs machine images, on a machine of the target's architecture.
- The Windows targets and `"win64"` stay as [0202](0202-the-windows-targets.md) decided them.

Alternatives rejected:

- **Linking with a Windows linker against import libraries:** a dependency on a Windows SDK, or on import libraries generated for it, for what the image writer already does for UEFI.
- **Sockets read and written on threads of their own:** a thread per request in flight, and a cancellation that closes the socket.
- **A completion port per executor thread:** the port of a completion would have to be chosen when the handle opens, before the thread waiting on it is known.

## Consequences

- Windows programs build on every machine and run where Wine runs on the target's architecture; a test run on a Windows target needs Wine.
- File operations block their executor thread, and a standard stream's read that the system cannot cancel ends when the system ends it.
- Wine sometimes reports a connection its peer reset as ended in order, so programs tested under Wine do not rely on resets.
