//! Wine prefixes for the harnesses: a session holds a prefix's server up, persistent, from the
//! prefix's setup until the session ends, so that Wine's own processes, which the setup starts
//! with their standard streams closed, neither start nor end while an image runs and write
//! nothing to the streams an image's run gives.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use nan::Launch;

/// The running server of a prefix, killed with every process of the prefix once dropped.
pub struct Session {
    prefix: PathBuf,
}

impl Session {
    /// Starts a persistent server for `prefix`, made when missing, then sets the prefix up with
    /// the Wine of `launch`.
    pub fn start(launch: &Launch, prefix: &Path) -> io::Result<Session> {
        fs::create_dir_all(prefix)?;
        let session = Session {
            prefix: prefix.to_owned(),
        };
        session.server("-p")?;
        quiet(
            Command::new(&launch.program)
                .args(["wineboot", "--init"])
                .envs(launch.env.iter().copied())
                .env("WINEPREFIX", prefix),
        )?;
        Ok(session)
    }

    fn server(&self, option: &str) -> io::Result<()> {
        quiet(
            Command::new("wineserver")
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

/// Runs a command to its end with its standard streams closed.
fn quiet(command: &mut Command) -> io::Result<()> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(drop)
}
