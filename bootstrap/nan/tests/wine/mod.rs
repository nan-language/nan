//! Wine prefixes for the harnesses: a session holds a prefix's server, the one its Wine starts,
//! up, persistent, from the prefix's setup until the session ends, so that Wine's own processes,
//! which the setup starts with their standard streams closed, neither start nor end while an
//! image runs and write nothing to the streams an image's run gives.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nan::Launch;

/// The running server of a prefix, killed with every process of the prefix once dropped.
pub struct Session {
    prefix: PathBuf,
    server: PathBuf,
}

impl Session {
    /// Sets `prefix` up, made when missing, with the Wine of `launch`, which starts the prefix's
    /// server as that Wine finds it; then restarts that server persistent and sets the prefix up
    /// again under it.
    pub fn start(launch: &Launch, prefix: &Path) -> io::Result<Session> {
        fs::create_dir_all(prefix)?;
        let setup = || {
            quiet(
                Command::new(&launch.program)
                    .args(["wineboot", "--init"])
                    .envs(launch.env.iter().copied())
                    .env("WINEPREFIX", prefix),
            )
        };
        setup()?;
        let session = Session {
            prefix: prefix.to_owned(),
            server: server_of(prefix)?,
        };
        session.server("-k")?;
        session.server("-w")?;
        session.server("-p")?;
        setup()?;
        Ok(session)
    }

    fn server(&self, option: &str) -> io::Result<()> {
        quiet(
            Command::new(&self.server)
                .arg(option)
                .env("WINEPREFIX", &self.prefix),
        )
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        drop(self.server("-k"));
        drop(self.server("-w"));
    }
}

/// The executable of the running server of `prefix`: a process whose executable's name starts
/// with `wineserver` and whose environment names the prefix.
fn server_of(prefix: &Path) -> io::Result<PathBuf> {
    let mut named = b"WINEPREFIX=".to_vec();
    named.extend_from_slice(prefix.as_os_str().as_encoded_bytes());
    for entry in fs::read_dir("/proc")? {
        let process = entry?.path();
        let Ok(executable) = fs::read_link(process.join("exe")) else {
            continue;
        };
        let server = executable
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("wineserver"));
        let environment = fs::read(process.join("environ")).unwrap_or_default();
        if server
            && environment
                .split(|byte| *byte == 0)
                .any(|variable| variable == named)
        {
            return Ok(executable);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!("no Wine server runs for {}", prefix.display()),
    ))
}

/// Runs a command to its end with its standard streams closed.
fn quiet(command: &mut Command) -> io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
}
