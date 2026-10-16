use std::io::Write as _;
use std::os::unix::process::CommandExt as _;

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let current = std::env::current_dir().expect("the current directory");
    let run = nan::run(&args, &current);
    if let Some(launch) = run.launch {
        let error = std::process::Command::new(&launch.image)
            .args(&launch.args)
            .exec();
        panic!("starting {}: {error}", launch.image.display());
    }
    let written = std::io::stdout().write_all(run.output.as_bytes());
    if let Err(error) = written
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        panic!("standard output: {error}");
    }
    std::process::exit(run.status);
}
