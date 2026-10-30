//! The emulator the machine targets' descriptors name: whether a target's images can run on this
//! machine, the invocation booting a machine image under the emulator, its boots with the TPM
//! emulator serving them, and boots bounded in host processor time.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::command::Launch;
use crate::workspace::{Arch, Kind, Target};

/// The firmware an `x86_64-uefi` image boots through, its code and the template of its
/// variables, and their fallbacks across distributions.
const OVMF: [(&str, &str); 4] = [
    (
        "/usr/share/edk2/ovmf/OVMF_CODE.fd",
        "/usr/share/edk2/ovmf/OVMF_VARS.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE.fd",
        "/usr/share/OVMF/OVMF_VARS.fd",
    ),
    (
        "/usr/share/OVMF/OVMF_CODE_4M.fd",
        "/usr/share/OVMF/OVMF_VARS_4M.fd",
    ),
    (
        "/usr/share/edk2/x64/OVMF_CODE.4m.fd",
        "/usr/share/edk2/x64/OVMF_VARS.4m.fd",
    ),
];

/// The firmware an `aarch64-uefi` image boots through, its code and the template of its
/// variables, each a flash image padded to the size of the `virt` board's flash, and their
/// fallbacks across distributions.
const AAVMF: [(&str, &str); 2] = [
    (
        "/usr/share/AAVMF/AAVMF_CODE.fd",
        "/usr/share/AAVMF/AAVMF_VARS.fd",
    ),
    (
        "/usr/share/edk2/aarch64/QEMU_EFI-pflash.raw",
        "/usr/share/edk2/aarch64/vars-template-pflash.raw",
    ),
];

/// The emulator program of an architecture's machine targets.
fn program(arch: Arch) -> &'static str {
    match arch {
        Arch::Amd64 => "qemu-system-x86_64",
        Arch::Aarch64 => "qemu-system-aarch64",
    }
}

/// The TPM emulator serving each boot's TPM.
const TPM_PROGRAM: &str = "swtpm";

/// The files of a machine directory, which the emulator and the TPM emulator run in: the disk,
/// the firmware's variables, the TPM's state directory, and the sockets of the TPM and of the
/// console port.
const DISK: &str = "disk.raw";
const VARS: &str = "vars.fd";
const TPM_STATE: &str = "tpm";
const TPM_SOCKET: &str = "tpm.sock";
const CONSOLE_SOCKET: &str = "console.sock";

/// The size of a machine's disk.
const DISK_BYTES: u64 = 256 << 20;

/// The status of a run under the emulator that ends other than by a write to the exit port.
pub const OTHER_END: i32 = 70;

/// The host processor time a bounded boot may use.
const PROCESSOR_TIME: Duration = Duration::from_secs(10);

/// The longest a bounded boot lasts: only a machine that waits forever without using its
/// processor time reaches it.
const WALL_TIME: Duration = Duration::from_secs(120);

/// How often a bounded boot's processor time is read, and the TPM emulator's socket looked for.
const POLL: Duration = Duration::from_millis(5);

/// The longest the TPM emulator may take to open its socket.
const TPM_WAIT: Duration = Duration::from_secs(10);

/// The clock ticks per second of the processor times `/proc` gives, USER_HZ.
const TICKS_PER_SECOND: u64 = 100;

/// The positions of a process's user and system times among the fields of its `/proc` stat that
/// follow its command name.
const USER_TIME: usize = 11;
const SYSTEM_TIME: usize = 12;

/// How a launch boots a machine.
pub enum Boots {
    /// The boots of a run: the machine's disk, firmware variables and TPM state persist in the
    /// machine directory `dir`, its console port is the socket there, and a reset reboots the
    /// machine when `resets` holds and ends the run otherwise.
    Kept { dir: PathBuf, resets: bool },
    /// The boots of a test run: each finds the machine as the launch made it, its network
    /// restricted to the machine and its console port unconnected, and a reset ends it.
    Fresh,
}

/// The emulator arguments common to every machine run of an architecture: the machine with its
/// IOMMU first, on x86_64 accelerated by KVM when `kvm` holds, processors, memory, entropy
/// device, exit port, TPM, watchdog, disk and network device, each virtio PCI device modern and
/// behind the IOMMU, no display, the serial port on the standard streams then the console port,
/// and an end with the process that starts it. `kept` holds whether a run's boots keep resets,
/// and is none for a test run's.
fn base_args(arch: Arch, kvm: bool, kept: Option<bool>) -> Vec<String> {
    let machine: &[&str] = match (arch, kvm) {
        (Arch::Amd64, true) => &[
            "-machine",
            "q35",
            "-device",
            "intel-iommu,intremap=on,aw-bits=48",
            "-accel",
            "kvm,kernel-irqchip=split",
            "-cpu",
            "host,+invtsc",
        ],
        (Arch::Amd64, false) => &[
            "-machine",
            "q35",
            "-device",
            "intel-iommu,intremap=on,aw-bits=48",
            "-accel",
            "tcg",
            "-cpu",
            "max",
        ],
        (Arch::Aarch64, _) => &[
            "-machine",
            "virt,gic-version=3,its=on,iommu=smmuv3",
            "-accel",
            "tcg",
            "-cpu",
            "cortex-a72",
        ],
    };
    let devices: &[&str] = match arch {
        Arch::Amd64 => &[
            "-device",
            "virtio-rng-pci,iommu_platform=on,disable-legacy=on",
            "-device",
            "isa-debug-exit,iobase=0xf4,iosize=0x01",
            "-device",
            "tpm-crb,tpmdev=tpm",
        ],
        Arch::Aarch64 => &[
            "-global",
            "virtio-mmio.force-legacy=false",
            "-device",
            "virtio-rng-device",
            "-semihosting-config",
            "enable=on,target=native",
            "-device",
            "tpm-tis-device,tpmdev=tpm",
        ],
    };
    let (disk, network, console) = match kept {
        Some(_) => (
            format!("if=none,id=disk,format=raw,file={DISK}"),
            "user,id=net".to_owned(),
            format!("unix:{CONSOLE_SOCKET},server=on,wait=off"),
        ),
        None => (
            format!("if=none,id=disk,format=raw,file={DISK},snapshot=on"),
            "user,id=net,restrict=on".to_owned(),
            "null".to_owned(),
        ),
    };
    let mut args: Vec<String> = ["-run-with", "exit-with-parent=on"]
        .iter()
        .chain(machine)
        .chain(&["-smp", "4", "-m", "512M"])
        .chain(devices)
        .chain(&[
            "-chardev",
            "socket,id=tpm,path=tpm.sock",
            "-tpmdev",
            "emulator,id=tpm,chardev=tpm",
            "-device",
            "i6300esb",
            "-device",
            "virtio-blk-pci,drive=disk,iommu_platform=on,disable-legacy=on",
            "-device",
            "virtio-net-pci,netdev=net,romfile=,iommu_platform=on,disable-legacy=on",
            "-display",
            "none",
            "-nodefaults",
            "-monitor",
            "none",
            "-serial",
            "stdio",
        ])
        .map(|argument| (*argument).to_owned())
        .collect();
    for (option, value) in [("-serial", console), ("-drive", disk), ("-netdev", network)] {
        args.push(option.to_owned());
        args.push(value);
    }
    if kept != Some(true) {
        args.push("-no-reboot".to_owned());
    }
    args
}

/// Whether the emulator accelerates an architecture's machines with KVM: x86_64 ones when this
/// machine is x86_64 and this process may read and write `/dev/kvm`.
fn kvm_usable(arch: Arch) -> bool {
    arch == Arch::Amd64
        && Arch::running() == arch
        && fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("/dev/kvm")
            .is_ok()
}

/// Whether a target's images can run on this machine: a host target's when the machine has its
/// architecture, and a machine target's when its emulator can start, the emulator program
/// running and able to end with the process that starts it, the TPM emulator running, and the
/// firmware present for a UEFI target.
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
    let tpm = Command::new(TPM_PROGRAM)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success());
    let (code, vars) = firmware(arch);
    runs && tpm
        && (target.kind() != Some(Kind::Uefi)
            || (Path::new(code).exists() && Path::new(vars).exists()))
}

/// Boots the emulator as `launch` gives on `machine`, with the standard streams of this process,
/// and gives the status the run ends with once the emulator ends.
pub fn run(launch: &Launch, machine: &Machine) -> i32 {
    let mut command = Command::new(&launch.program);
    command.args(&launch.args);
    run_status(
        machine
            .boot(command)
            .and_then(|mut boot| boot.emulator.wait()),
    )
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

/// The firmware of an architecture's UEFI target, its code and the template of its variables:
/// the first pair present on the host, or the first pair when none is.
fn firmware(arch: Arch) -> (&'static str, &'static str) {
    let pairs: &[(&'static str, &'static str)] = match arch {
        Arch::Amd64 => &OVMF,
        Arch::Aarch64 => &AAVMF,
    };
    pairs
        .iter()
        .copied()
        .find(|(code, vars)| Path::new(code).exists() && Path::new(vars).exists())
        .unwrap_or(pairs[0])
}

/// The machine directory a launch boots in, holding the machine's disk, firmware variables, TPM
/// state and sockets, and whether every boot makes its TPM state anew.
pub struct Machine {
    pub dir: PathBuf,
    fresh: bool,
}

/// A boot of the emulator, and the TPM emulator serving it; dropping it ends both.
pub struct Boot {
    pub emulator: Child,
    tpm: Child,
}

impl Machine {
    /// Starts the TPM emulator on the machine's TPM state, made anew for a fresh machine, waits
    /// for its socket, then starts `command`, both in the machine directory.
    pub fn boot(&self, mut command: Command) -> io::Result<Boot> {
        let state = self.dir.join(TPM_STATE);
        if self.fresh && state.exists() {
            fs::remove_dir_all(&state)?;
        }
        fs::create_dir_all(&state)?;
        let socket = self.dir.join(TPM_SOCKET);
        if socket.exists() {
            fs::remove_file(&socket)?;
        }
        let mut tpm = Command::new(TPM_PROGRAM)
            .args(["socket", "--tpm2", "--tpmstate"])
            .arg(format!("dir={TPM_STATE}"))
            .args(["--ctrl"])
            .arg(format!("type=unixio,path={TPM_SOCKET}"))
            .arg("--terminate")
            .current_dir(&self.dir)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let started = Instant::now();
        let listening = loop {
            if socket.exists() {
                break Ok(());
            }
            match tpm.try_wait() {
                Ok(None) if started.elapsed() < TPM_WAIT => thread::sleep(POLL),
                Ok(_) => break Err(io::Error::other("the TPM emulator opens no socket")),
                Err(error) => break Err(error),
            }
        };
        let emulator = listening.and_then(|()| command.current_dir(&self.dir).spawn());
        match emulator {
            Ok(emulator) => Ok(Boot { emulator, tpm }),
            Err(error) => {
                drop(tpm.kill());
                drop(tpm.wait());
                Err(error)
            }
        }
    }
}

impl Drop for Boot {
    fn drop(&mut self) {
        for child in [&mut self.emulator, &mut self.tpm] {
            drop(child.kill());
            drop(child.wait());
        }
    }
}

/// The emulator invocation that boots a machine image, in a machine directory holding a disk
/// made blank when missing: a bare image directly, by the PVH boot protocol on x86_64 and by
/// loading its segments on AArch64, and a UEFI image through the firmware from a FAT volume
/// holding it at the default boot path and each of `files` at `/` followed by its name.
pub fn launch(target: Target, image: &Path, files: &[PathBuf], boots: Boots) -> io::Result<Launch> {
    let arch = target.arch().expect("a machine target's architecture");
    let (dir, kept) = match boots {
        Boots::Kept { dir, resets } => (dir, Some(resets)),
        Boots::Fresh => (machine_of(image), None),
    };
    fs::create_dir_all(&dir)?;
    let disk = dir.join(DISK);
    if !disk.exists() {
        fs::File::create(&disk)?.set_len(DISK_BYTES)?;
    }
    let mut args = base_args(arch, kvm_usable(arch), kept);
    match target.kind() {
        Some(Kind::Uefi) => {
            let (code, template) = firmware(arch);
            let volume = boot_volume(arch, image, files)?.display().to_string();
            let (vars, volume) = match kept {
                Some(_) => {
                    let vars = dir.join(VARS);
                    if !vars.exists() {
                        fs::copy(template, vars)?;
                    }
                    (
                        format!("if=pflash,format=raw,unit=1,file={VARS}"),
                        format!("if=none,id=boot,format=raw,file=fat:rw:{volume}"),
                    )
                }
                None => (
                    format!("if=pflash,format=raw,unit=1,file={template},snapshot=on"),
                    format!("if=none,id=boot,format=raw,file=fat:{volume},snapshot=on"),
                ),
            };
            for (option, value) in [
                (
                    "-drive",
                    format!("if=pflash,format=raw,unit=0,readonly=on,file={code}"),
                ),
                ("-drive", vars),
                ("-drive", volume),
                (
                    "-device",
                    "virtio-blk-pci,drive=boot,bootindex=0,iommu_platform=on,disable-legacy=on"
                        .to_owned(),
                ),
            ] {
                args.push(option.to_owned());
                args.push(value);
            }
        }
        _ => {
            args.push("-kernel".to_owned());
            args.push(image.display().to_string());
        }
    }
    Ok(Launch {
        program: program(arch).into(),
        args,
        machine: Some(Machine {
            dir,
            fresh: kept.is_none(),
        }),
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

/// The machine directory of a test run's launch of an image.
fn machine_of(image: &Path) -> PathBuf {
    image.with_extension("machine")
}

/// Removes a test image with the boot volume and machine directory a launch made for it.
pub fn discard(image: &Path) -> io::Result<()> {
    for directory in [volume_of(image), machine_of(image)] {
        if directory.exists() {
            fs::remove_dir_all(directory)?;
        }
    }
    fs::remove_file(image)
}

/// Boots the emulator as `launch` gives, with the file at `path` as its configuration file
/// `name`, and gives how it ended, its serial output and its diagnostics; none once it has used
/// more than `PROCESSOR_TIME` of host processor time, or lasted `WALL_TIME`.
pub fn boot(launch: &Launch, (name, path): (&str, &Path)) -> io::Result<Option<Output>> {
    let machine = launch.machine.as_ref().expect("a machine image's launch");
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .arg("-fw_cfg")
        .arg(format!("name={name},file={}", path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut boot = machine.boot(command)?;
    let child = &mut boot.emulator;
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
