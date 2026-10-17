use std::os::unix::process::ExitStatusExt as _;

use super::*;

#[test]
fn a_run_ends_with_the_byte_written_to_the_exit_port_and_otherwise_with_70() {
    let exited = |code: i32| Ok(ExitStatus::from_raw(code << 8));
    assert_eq!(run_status(exited(1)), 0);
    assert_eq!(run_status(exited(141)), 70);
    assert_eq!(run_status(exited(255)), 127);
    assert_eq!(run_status(exited(0)), OTHER_END);
    assert_eq!(run_status(Ok(ExitStatus::from_raw(9))), OTHER_END);
    assert_eq!(run_status(Err(io::ErrorKind::NotFound.into())), OTHER_END);
}
