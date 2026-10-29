//! The emulator the machine targets' descriptors name: whether a target's images can run on this
//! machine, the invocation booting a machine image under the emulator, and boots bounded in host
//! processor time.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::command::Launch;
use crate::workspace::{Arch, Kind, Target};

/// The firmware an `x86_64-uefi` image boots through, and its fallbacks across distributions.
const OVMF: [&str; 4] = [
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
];

/// The firmware an `aarch64-uefi` image boots through, each a flash image padded to the size of
/// the `virt` board's flash, and its fallbacks across distributions.
const AAVMF: [&str; 2] = [
    "/usr/share/AAVMF/AAVMF_CODE.fd",
    "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
];

/// The emulator program of an architecture's machine targets.
fn program(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "qemu-system-x86_64",
        Arch::Aarch64 => "qemu-system-aarch64",
    }
}

/// The status of a run under the emulator that ends other than by a write to the exit port.
pub const OTHER_END: i32 = 70;

/// The host processor time a bounded boot may use.
const PROCESSOR_TIME: Duration = Duration::from_secs(10);

/// The longest a bounded boot lasts: only a machine that waits forever without using its
/// processor time reaches it.
const WALL_TIME: Duration = Duration::from_secs(120);

/// How often a bounded boot's processor time is read.
const POLL: Duration = Duration::from_millis(5);

/// The clock ticks per second of the processor times `/proc` gives, USER_HZ.
const TICKS_PER_SECOND: u64 = 100;

/// The positions of a process's user and system times among the fields of its `/proc` stat that
/// follow its command name.
const USER_TIME: usize = 11;
const SYSTEM_TIME: usize = 12;

/// The emulator arguments common to every machine run of an architecture: the machine, on x86_64
/// accelerated by KVM where the host offers it, processors, memory, entropy device, exit port,
/// serial port on the standard streams, no display or reboot, and an end with the process that
/// starts it.
fn base_args(arch: Arch) -> Vec<String> {
    let machine: &[&str] = match arch {
        Arch::Amd64 => &[
            "-machine",
            "q35",
            "-accel",
            "kvm",
            "-accel",
            "tcg",
            "-device",
            "virtio-rng-pci",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x01",
        ],
        Arch::Aarch64 => &[
            "-machine",
            "virt,gic-version=3",
            "-cpu",
            "cortex-a72",
            "-accel",
            "tcg",
            "-global",
            "virtio-mmio.force-legacy=false",
            "-device",
            "virtio-rng-device",
            "-semihosting-config",
            "enable=on,target=native",
        ],
    };
    [
        "-run-with",
        "exit-with-parent=on",
        "-smp",
        "4",
        "-m",
        "512M",
    ]
    .iter()
    .chain(machine)
    .chain(&[
        "-display",
        "none",
        "-no-reboot",
        "-nodefaults",
        "-serial",
        "stdio",
        "-monitor",
        "none",
    ])
    .map(|argument| (*argument).to_owned())
    .collect()
}

/// Whether a target's images can run on this machine: a host target's when the machine has its
/// architecture, and a machine target's when its emulator can start, the emulator program
/// running and able to end with the process that starts it, and the firmware present for a UEFI
/// target.
pub fn available(target: Target) -> bool {
    let Some(arch) = target.arch() else {
        return false;
    };
    if target.is_host() {
        return arch == Arch::running();
    }
    let runs = Command::new(program(arch))
        .args(["-run-with", "help"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("exit-with-parent"));
    runs && (target.kind() != Some(Kind::Uefi) || Path::new(firmware(arch)).exists())
}

/// Boots the emulator as `launch` gives, with the standard streams of this process, and gives
/// the status the run ends with once the emulator ends.
pub fn run(launch: &Launch) -> i32 {
    run_status(Command::new(&launch.program).args(&launch.args).status())
}

/// The status a run under the emulator ends with: v mod 128 when the machine wrote the byte v to
/// the exit port, which ends the emulator with status (2v + 1) mod 256, and `OTHER_END` for any
/// other end.
fn run_status(ended: io::Result<ExitStatus>) -> i32 {
    match ended.map(|status| status.code()) {
        Ok(Some(code)) if code % 2 == 1 => (code - 1) / 2,
        _ => OTHER_END,
    }
}

/// The firmware of an architecture's UEFI target: the first of its paths present on the host,
/// or the first of them when none is.
fn firmware(arch: Arch) -> &'static str {
    let paths: &[&'static str] = match arch {
        Arch::Amd64 => &OVMF,
        Arch::Aarch64 => &AAVMF,
    };
    paths
        .iter()
        .copied()
        .find(|path| Path::new(path).exists())
        .unwrap_or(paths[0])
}

/// What becomes of what a machine writes to its boot volume.
pub enum Writes {
    /// The volume's directory keeps it.
    Kept,
    /// It lasts until the emulator ends, so that every boot finds the volume as it was made.
    Dropped,
}

/// The emulator invocation that boots a machine image: a bare image directly, by the PVH boot
/// protocol on x86_64 and by loading its segments on AArch64, and a UEFI image through the
/// firmware from a FAT volume holding it at the default boot path and each of `files` at `/`
/// followed by its name.
pub fn launch(
    target: Target,
    image: &Path,
    files: &[PathBuf],
    writes: Writes,
) -> io::Result<Launch> {
    let arch = target.arch().expect("a machine target's architecture");
    let mut args = base_args(arch);
    match target.kind() {
        Some(Kind::Uefi) => {
            let firmware = firmware(arch);
            let volume = boot_volume(arch, image, files)?.display().to_string();
            args.push("-drive".to_owned());
            args.push(format!(
                "if=pflash,format=raw,unit=0,readonly=on,file={firmware}"
            ));
            args.push("-drive".to_owned());
            args.push(match writes {
                Writes::Kept => format!("format=raw,file=fat:rw:{volume}"),
                Writes::Dropped => format!("format=raw,file=fat:{volume},snapshot=on"),
            });
        }
        _ => {
            args.push("-kernel".to_owned());
            args.push(image.display().to_string());
        }
    }
    Ok(Launch {
        program: program(arch).into(),
        args,
        emulated: true,
    })
}

/// The FAT boot volume of a UEFI image: a directory beside it holding the image as the default
/// boot path the firmware of its architecture loads, and each of `files` at its root.
fn boot_volume(arch: Arch, image: &Path, files: &[PathBuf]) -> io::Result<PathBuf> {
    let directory = volume_of(image);
    if directory.exists() {
        fs::remove_dir_all(&directory)?;
    }
    let boot = directory.join("EFI").join("BOOT");
    fs::create_dir_all(&boot)?;
    let default = match arch {
        Arch::Amd64 => "BOOTX64.EFI",
        Arch::Aarch64 => "BOOTAA64.EFI",
    };
    fs::copy(image, boot.join(default))?;
    for file in files {
        let name = file.file_name().expect("an image file has a name");
        fs::copy(file, directory.join(name))?;
    }
    Ok(directory)
}

/// The directory a launch makes the boot volume of an image in.
fn volume_of(image: &Path) -> PathBuf {
    image.with_extension("boot")
}

/// Removes a machine image and any boot volume a launch made for it.
pub fn discard(image: &Path) -> io::Result<()> {
    let volume = volume_of(image);
    if volume.exists() {
        fs::remove_dir_all(volume)?;
    }
    fs::remove_file(image)
}

/// Boots the emulator as `launch` gives, with the file at `path` as its configuration file
/// `name`, and gives how it ended, its serial output and its diagnostics; none once it has used
/// more than `PROCESSOR_TIME` of host processor time, or lasted `WALL_TIME`.
pub fn boot(launch: &Launch, (name, path): (&str, &Path)) -> io::Result<Option<Output>> {
    let mut child = Command::new(&launch.program)
        .args(&launch.args)
        .arg("-fw_cfg")
        .arg(format!("name={name},file={}", path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let stdout = drain(child.stdout.take().expect("the emulator's piped output"));
    let stderr = drain(
        child
            .stderr
            .take()
            .expect("the emulator's piped diagnostics"),
    );
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Some(status);
        }
        let within = processor_time(child.id())
            .map(|used| used <= PROCESSOR_TIME && started.elapsed() < WALL_TIME);
        if !matches!(within, Ok(true)) {
            child.kill()?;
            child.wait()?;
            within?;
            break None;
        }
        thread::sleep(POLL);
    };
    let stdout = stdout.join().expect("the emulator's output");
    let stderr = stderr.join().expect("the emulator's diagnostics");
    Ok(status.map(|status| Output {
        status,
        stdout,
        stderr,
    }))
}

/// The host processor time a process has used, its threads' in user and system mode.
fn processor_time(pid: u32) -> io::Result<Duration> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<&str> = stat
        .rsplit_once(')')
        .map_or("", |(_, fields)| fields)
        .split_whitespace()
        .collect();
    let ticks: Option<u64> = [USER_TIME, SYSTEM_TIME]
        .iter()
        .map(|at| fields.get(*at)?.parse::<u64>().ok())
        .sum();
    let ticks = ticks.ok_or_else(|| io::Error::other(format!("/proc/{pid}/stat reads {stat}")))?;
    Ok(Duration::from_millis(ticks * 1_000 / TICKS_PER_SECOND))
}

/// Reads a stream to its end on a thread of its own, giving its bytes when joined.
fn drain(mut stream: impl Read + Send + 'static) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = stream.read_to_end(&mut bytes);
        bytes
    })
}

#[cfg(test)]
mod tests;
