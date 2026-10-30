//! Machine programs under the emulator, for the harnesses: the boot of the image `nan run`
//! launches, until the emulator ends, with what the program writes to the serial port and the
//! status `nan run` ends with.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// How long a booted machine program may run before it ends through the exit port.
const BOOT_LIMIT: Duration = Duration::from_secs(60);

/// A machine program's boot: the status `nan run` ends with, the byte the program wrote to the
/// exit port or 70 when the emulator ended otherwise, and what the program wrote to the serial
/// port.
pub struct Booted {
    pub status: i32,
    pub written: String,
}

/// Boots the image `launch` starts until the emulator ends, failing when it runs past
/// `BOOT_LIMIT`.
pub fn boot(launch: &nan::Launch) -> Result<Booted, String> {
    let machine = launch
        .machine
        .as_ref()
        .ok_or("an image launched without the emulator")?;
    let mut command = Command::new(&launch.program);
    command
        .args(&launch.args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let mut boot = machine
        .boot(command)
        .map_err(|error| format!("starting {}: {error}", launch.program.display()))?;
    let emulator = &mut boot.emulator;
    let mut serial = emulator.stdout.take().expect("the serial port's output");
    let reader = thread::spawn(move || {
        let mut bytes = Vec::new();
        drop(serial.read_to_end(&mut bytes));
        bytes
    });
    let deadline = Instant::now() + BOOT_LIMIT;
    let ended = loop {
        if let Some(status) = emulator.try_wait().map_err(|error| error.to_string())? {
            break Some(status);
        }
        if Instant::now() >= deadline {
            drop(emulator.kill());
            drop(emulator.wait());
            break None;
        }
        thread::sleep(Duration::from_millis(20));
    };
    let written = reader.join().expect("the serial port's reader");
    let written = String::from_utf8_lossy(&written).into_owned();
    let Some(ended) = ended else {
        return Err(format!(
            "the program runs past {BOOT_LIMIT:?}, writing\n{written}"
        ));
    };
    let status = match ended.code() {
        Some(code) if code % 2 == 1 => (code - 1) / 2,
        _ => 70,
    };
    Ok(Booted { status, written })
}
