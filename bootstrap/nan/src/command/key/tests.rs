use std::fs;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::PathBuf;

use crate::command::tests::{MANIFEST, run_in};
use crate::sign;
use crate::workspace::tests::Scratch;

/// A principal of this process's tests, named `name`, whose secret key the test home directory
/// holds from `seed` when given; its key file is removed when dropped.
pub(crate) struct TestPrincipal(pub String);

impl TestPrincipal {
    pub(crate) fn new(name: &str, seed: Option<[u8; 32]>) -> TestPrincipal {
        let principal = TestPrincipal(format!("{name}-{}", std::process::id()));
        if let Some(seed) = seed {
            let keys = std::env::temp_dir().join("nan-test-home/.nan/keys");
            let made = fs::DirBuilder::new().mode(0o700).create(&keys);
            assert!(made.is_ok() || keys.is_dir(), "the keys directory");
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(principal.file())
                .expect("a key file");
            std::io::Write::write_all(&mut file, &seed).expect("the seed written");
        }
        principal
    }

    fn file(&self) -> PathBuf {
        std::env::temp_dir().join(format!("nan-test-home/.nan/keys/{}.key", self.0))
    }

    /// The text form of the public key of a seed.
    pub(crate) fn key_of(seed: [u8; 32]) -> String {
        sign::key_form(&sign::public_key(&seed))
    }
}

impl Drop for TestPrincipal {
    fn drop(&mut self) {
        let _ = fs::remove_file(self.file());
    }
}

#[test]
fn a_key_is_made_once_shown_and_trusted_by_the_current_package() {
    let scratch = Scratch::new(&[("app/package.nan", MANIFEST)]);
    let dir = &scratch.0;
    let principal = TestPrincipal::new("keeper", None);
    let name = &principal.0;
    let made = run_in(dir, &format!("key new {name}")).output;
    let key = made
        .lines()
        .nth(1)
        .and_then(|line| line.strip_prefix(&format!("key {name} ")))
        .expect("the public key")
        .to_owned();
    assert!(
        made.contains(&format!("\nwrote ~/.nan/keys/{name}.key\n")),
        "{made}"
    );
    assert!(
        run_in(dir, &format!("key new {name}"))
            .output
            .contains("\nerror key.exists - -\n")
    );
    assert_eq!(
        run_in(dir, &format!("key show {name}")).output,
        format!("ok key show {name}: records 1\nkey {name} {key}\nok key show {name}: records 1\n")
    );
    let absent = TestPrincipal::new("absent", None);
    assert!(
        run_in(dir, &format!("key show {}", absent.0))
            .output
            .contains("\nerror key.missing - -\n")
    );
    assert!(
        run_in(dir, "key show bad/name")
            .output
            .contains("\nerror seal.bad-principal - -\n")
    );
    let app = dir.join("app");
    assert!(
        run_in(&app, &format!("key trust {name} {}", &key[1..]))
            .output
            .contains("\nerror key.bad-form - -\n")
    );
    let trusted = run_in(&app, &format!("key trust {name} {key}")).output;
    assert!(
        trusted.contains("\nwrote app/.nan/principals.nan\n"),
        "{trusted}"
    );
    let file = fs::read_to_string(app.join(".nan/principals.nan")).expect("the principals file");
    assert!(file.contains(&format!("\nprincipal \"{name}\" \"{key}\"\n")));
    assert!(
        run_in(&app, &format!("key trust {name} {key}"))
            .output
            .starts_with(&format!("ok key trust {name} {key}: files 0,"))
    );
}
