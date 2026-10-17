//! The emulator the machine targets' descriptors name: whether it can start, the invocation
//! booting a machine image under it, and boots bounded in host processor time.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::command::Launch;
use crate::workspace::Target;

/// The emulator program.
const QEMU: &str = "qemu-system-x86_64";

/// The firmware an `x86_64-uefi` image boots through, and its fallbacks across distributions.
const OVMF: [&str; 4] = [
    "/usr/share/edk2/ovmf/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE.fd",
    "/usr/share/OVMF/OVMF_CODE_4M.fd",
    "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
];

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

/// The emulator arguments common to every machine run: the machine, accelerated by KVM where the
/// host offers it, processors, memory, entropy device, exit port, serial port on the standard
/// streams, no display or reboot, and an end with the process that starts it.
fn base_args() -> Vec<String> {
    [
        "-run-with",
        "exit-with-parent=on",
        "-machine",
        "q35",
        "-accel",
        "kvm",
        "-accel",
        "tcg",
        "-smp",
        "4",
        "-m",
        "512M",
        "-device",
        "virtio-rng-pci",
        "-device",
        "isa-debug-exit,iobase=0xf4,iosize=0x01",
        "-display",
        "none",
        "-no-reboot",
        "-nodefaults",
        "-serial",
        "stdio",
        "-monitor",
        "none",
    ]
    .iter()
    .map(|argument| (*argument).to_owned())
    .collect()
}

/// Whether the emulator of a machine target can start: the emulator program runs and can end
/// with the process that starts it, and OVMF is present for `x86_64-uefi`.
pub fn available(target: Target) -> bool {
    let runs = Command::new(QEMU)
        .args(["-run-with", "help"])
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|output| String::from_utf8_lossy(&output.stdout).contains("exit-with-parent"));
    runs && (target != Target::Uefi || firmware().is_some())
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

/// The OVMF firmware present on the host.
fn firmware() -> Option<&'static str> {
    OVMF.iter().copied().find(|path| Path::new(path).exists())
}

/// What becomes of what a machine writes to its boot volume.
pub enum Writes {
    /// The volume's directory keeps it.
    Kept,
    /// It lasts until the emulator ends, so that every boot finds the volume as it was made.
    Dropped,
}

/// The emulator invocation that boots a machine image: an `x86_64-bare` image directly through
/// the PVH boot protocol, an `x86_64-uefi` image through OVMF from a FAT volume holding it as
/// `/EFI/BOOT/BOOTX64.EFI` and each of `files` at `/` followed by its name.
pub fn launch(
    target: Target,
    image: &Path,
    files: &[PathBuf],
    writes: Writes,
) -> io::Result<Launch> {
    let mut args = base_args();
    match target {
        Target::Uefi => {
            let firmware = firmware().unwrap_or(OVMF[0]);
            let volume = boot_volume(image, files)?.display().to_string();
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
        program: QEMU.into(),
        args,
        emulated: true,
    })
}

/// The FAT boot volume of a UEFI image: a directory beside it holding the image as the default
/// boot path the firmware loads, and each of `files` at its root.
fn boot_volume(image: &Path, files: &[PathBuf]) -> io::Result<PathBuf> {
    let directory = volume_of(image);
    if directory.exists() {
        fs::remove_dir_all(&directory)?;
    }
    let boot = directory.join("EFI").join("BOOT");
    fs::create_dir_all(&boot)?;
    fs::copy(image, boot.join("BOOTX64.EFI"))?;
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
