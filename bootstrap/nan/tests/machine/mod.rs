//! Machine programs under the emulator, for the harnesses: the boot of the image `nan run`
//! launches, until the emulator ends, with what the program writes to the serial port and the
//! status `nan run` ends with, and the requests made on the run's monitor as the program writes.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
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

/// A command of QEMU's machine protocol made on a run's monitor once the program has written
/// the line `after` to the serial port.
pub struct Request {
    pub after: String,
    pub command: String,
}

/// Boots the image `launch` starts until the emulator ends, making each of `requests` in turn,
/// failing when it runs past `BOOT_LIMIT` or the monitor refuses a request.
pub fn boot(launch: &nan::Launch, requests: Vec<Request>) -> Result<Booted, String> {
    let machine = launch
        .machine
        .as_ref()
        .ok_or("an image launched without the emulator")?;
    let dir = File::open(&machine.dir).map_err(|error| error.to_string())?;
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
    let serial = emulator.stdout.take().expect("the serial port's output");
    let reader = thread::spawn(move || serve(serial, &dir, requests));
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
    let (written, refused) = reader.join().expect("the serial port's reader");
    let written = String::from_utf8_lossy(&written).into_owned();
    if let Some(refused) = refused {
        return Err(format!("{refused}, the program writing\n{written}"));
    }
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

/// Reads the serial port to its end, making each request on the monitor of the machine
/// directory `dir` once the line it follows is written, and gives what was written with the
/// first refusal of a request.
fn serve(mut serial: impl Read, dir: &File, requests: Vec<Request>) -> (Vec<u8>, Option<String>) {
    let mut written = Vec::new();
    let mut searched = 0;
    let mut monitor = None;
    let mut refused = None;
    let mut requests = requests.into_iter().peekable();
    let mut chunk = [0; 4096];
    while let Ok(read @ 1..) = serial.read(&mut chunk) {
        written.extend_from_slice(&chunk[..read]);
        while let Some(request) = requests.peek() {
            let line = format!("{}\n", request.after).into_bytes();
            let Some(at) = written[searched..]
                .windows(line.len())
                .position(|window| window == line)
            else {
                break;
            };
            searched += at + line.len();
            if refused.is_none() {
                refused = make(&mut monitor, dir, &request.command).err();
            }
            requests.next();
        }
    }
    (written, refused)
}

/// A run's monitor: QEMU's machine protocol on the socket of its machine directory.
struct Monitor {
    replies: BufReader<UnixStream>,
    commands: UnixStream,
}

/// Makes a command on the monitor of the machine directory `dir`, connecting to it first, by a
/// path short enough for a socket's name.
fn make(monitor: &mut Option<Monitor>, dir: &File, command: &str) -> Result<(), String> {
    let monitor = match monitor {
        Some(monitor) => monitor,
        None => {
            let path = format!("/proc/self/fd/{}/monitor.sock", dir.as_raw_fd());
            let commands = UnixStream::connect(path).map_err(|error| error.to_string())?;
            let replies = commands.try_clone().map_err(|error| error.to_string())?;
            let connected = monitor.insert(Monitor {
                replies: BufReader::new(replies),
                commands,
            });
            connected.reply()?;
            connected.send(r#"{"execute":"qmp_capabilities"}"#)?;
            connected
        }
    };
    monitor.send(command)
}

impl Monitor {
    /// Sends a command and waits for its reply, failing when it is an error.
    fn send(&mut self, command: &str) -> Result<(), String> {
        writeln!(self.commands, "{command}").map_err(|error| error.to_string())?;
        let reply = self.reply()?;
        match reply.starts_with(r#"{"return""#) {
            true => Ok(()),
            false => Err(format!("the monitor answers {command} with {reply}")),
        }
    }

    /// The next line the monitor writes other than an event.
    fn reply(&mut self) -> Result<String, String> {
        loop {
            let mut line = String::new();
            match self.replies.read_line(&mut line) {
                Ok(0) => return Err("the monitor closes".to_owned()),
                Ok(_) if line.starts_with(r#"{"timestamp""#) => {}
                Ok(_) => return Ok(line.trim_end().to_owned()),
                Err(error) => return Err(error.to_string()),
            }
        }
    }
}
