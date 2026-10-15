use std::io::Write as _;

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let current = std::env::current_dir().expect("the current directory");
    let run = nan::run(&args, &current);
    let written = std::io::stdout().write_all(run.output.as_bytes());
    if let Err(error) = written
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        panic!("standard output: {error}");
    }
    std::process::exit(run.status);
}
