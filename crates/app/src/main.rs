#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use std::{path::PathBuf, process::ExitCode};
    let mut arguments = std::env::args_os().skip(1);
    let first = arguments.next();
    if first
        .as_deref()
        .is_some_and(|argument| argument == "--help" || argument == "-h")
    {
        println!(
            "usage: apteronotus [source.eod]\n\nOpen a source document without evaluating it. Run starts audio.\nCtrl/Cmd+O opens a file; Ctrl/Cmd+S saves it."
        );
        return ExitCode::SUCCESS;
    }
    if arguments.next().is_some() {
        eprintln!("apteronotus: expected at most one source file; use --help");
        return ExitCode::FAILURE;
    }
    let path = first.map(PathBuf::from);
    match apteronotus_app::run_native_file(path.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("apteronotus: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn main() {}
