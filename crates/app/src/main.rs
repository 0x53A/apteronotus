#[cfg(not(target_arch = "wasm32"))]
#[derive(Debug,Default,PartialEq)]
struct Arguments {
    source: Option<std::path::PathBuf>,
}
#[cfg(not(target_arch = "wasm32"))]
fn parse(arguments:impl IntoIterator<Item=std::ffi::OsString>)->Result<Option<Arguments>,String> {
    let mut arguments=arguments.into_iter();let mut parsed=Arguments::default();let mut positional=false;
    while let Some(argument)=arguments.next() {
        if !positional && (argument=="--help" || argument=="-h") {return Ok(None);}
        if !positional && argument=="--" {positional=true;continue;}
        if !positional && argument.to_string_lossy().starts_with('-') {return Err(format!("unknown option {:?}; use --help",argument));}
        if parsed.source.replace(argument.into()).is_some() {return Err("expected at most one source file; use --help".into());}
    }
    Ok(Some(parsed))
}
#[cfg(not(target_arch = "wasm32"))]
fn main() -> std::process::ExitCode {
    use std::process::ExitCode;
    let parsed=match parse(std::env::args_os().skip(1)) {
        Ok(Some(parsed))=>parsed,
        Ok(None)=>{
            println!("usage: apteronotus [source.eod]

Open a source document without evaluating it. Run starts audio.");
            return ExitCode::SUCCESS;
        }
        Err(error)=>{eprintln!("apteronotus: {error}");return ExitCode::FAILURE;}
    };
    match apteronotus_app::run_native_file(parsed.source.as_deref()) {
        Ok(())=>ExitCode::SUCCESS,
        Err(error)=>{eprintln!("apteronotus: {error}");ExitCode::FAILURE}
    }
}
#[cfg(target_arch = "wasm32")]
fn main() {}

#[cfg(all(test,not(target_arch="wasm32")))]
mod tests {
    use super::*;
    fn args(a:&[&str])->Result<Option<Arguments>,String> {parse(a.iter().map(std::ffi::OsString::from))}
    #[test]
    fn source_and_help_arguments() {
        assert_eq!(args(&["song.eod"]).unwrap(),Some(Arguments{source:Some("song.eod".into())}));
        assert!(args(&[]).unwrap().unwrap().source.is_none());
        assert_eq!(args(&["--","-song.eod"]).unwrap().unwrap().source,Some("-song.eod".into()));
        assert_eq!(args(&["--help"]).unwrap(),None);
    }
    #[test]
    fn malformed_arguments_are_rejected_before_opening_anything() {
        for a in [&["one","two"][..],&["--unknown"],&["--prepared-violin","a","b"]] {assert!(args(a).is_err());}
    }
}
