use std::io::Write as _;

fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let current = std::env::current_dir().expect("the current directory");
    let run = nan::run(&args, &current);
    std::io::stdout()
        .write_all(run.output.as_bytes())
        .expect("standard output");
    std::process::exit(run.status);
}
