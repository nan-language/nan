use std::io::Write as _;

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let run = match std::env::current_dir() {
        Ok(current) => nan::run(&args, &current),
        Err(_) => nan::without_directory(&args),
    };
    if let Some(launch) = run.launch {
        std::process::exit(launch.start());
    }
    let written = std::io::stdout().write_all(run.output.as_bytes());
    if let Err(error) = written
        && error.kind() != std::io::ErrorKind::BrokenPipe
    {
        eprintln!("nan: standard output does not write");
        std::process::exit(3);
    }
    std::process::exit(run.status);
}
