use std::process::Command;
use std::thread::sleep;
use std::time::{Duration, Instant};

use crate::workspace::tests::Scratch;

/// Sends `SIGTERM` to a process.
fn terminate(pid: u32) {
    let sent = Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("run kill");
    assert!(sent.success(), "kill {pid}");
}

#[test]
fn a_second_termination_request_ends_a_program_that_never_waits() {
    let scratch = Scratch::new(&[
        (
            "spin/package.nan",
            "//! Counts forever.\n\npackage spin {\n    language 1\n    targets x86_64-host\n    entry main\n} spin\n",
        ),
        (
            "spin/spin.nan",
            "//! A count that never ends.\n\nfn main(take world: host::World) -> u8 fails task::Cancelled uses wait {\n    var count: u64 = 0\n    loop {\n        count = count.wrapping_add(1)\n    }\n} main\n",
        ),
    ]);
    let built = crate::run(&["build".to_owned()], &scratch.0);
    assert_eq!(built.status, 0, "{}", built.output);
    let mut child = Command::new(scratch.0.join(".nan/images/x86_64-host/spin"))
        .spawn()
        .expect("start the program");
    sleep(Duration::from_millis(300));
    terminate(child.id());
    sleep(Duration::from_millis(300));
    terminate(child.id());
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait for the program") {
            break Some(status);
        }
        if Instant::now() >= deadline {
            drop(child.kill());
            drop(child.wait());
            break None;
        }
        sleep(Duration::from_millis(10));
    };
    assert_eq!(status.and_then(|status| status.code()), Some(143));
}
