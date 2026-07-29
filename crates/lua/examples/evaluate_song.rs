use apteronotus_lua::evaluate;

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: evaluate_song <song.eod>");
    let source = std::fs::read_to_string(&path).expect("could not read song");
    match evaluate(&source) {
        Ok(program) => println!(
            "{path}: playable candidate (voices {}, tracks {}, runs {})",
            program.voices.len(),
            program.tracks.len(),
            program.runs.len()
        ),
        Err(error) => {
            eprintln!("{path}: {error}");
            std::process::exit(1);
        }
    }
}
