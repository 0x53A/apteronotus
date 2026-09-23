//! Exercise the installed-binary interface from a directory without sources.
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "apteronotus-cli-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn run(&self, arguments: &[&str]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_apteronotus-render"))
            .current_dir(&self.0)
            .args(arguments)
            .output()
            .unwrap()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn succeeds(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn wav_samples(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<f32>()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn corpus_listing_and_track_listing_need_no_checkout_and_write_nothing() {
    let directory = Directory::new();
    let listed = directory.run(&["--list-songs"]);
    succeeds(&listed);
    let text = String::from_utf8(listed.stdout).unwrap();
    for song in apteronotus_songs::SONGS {
        assert!(
            text.lines()
                .any(|line| line.starts_with(&format!("{} — ", song.name)))
        );
    }
    succeeds(&directory.run(&["--song", "nightshift-dub", "--list-tracks"]));
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn embedded_and_file_sources_render_identical_audio_and_refuse_overwrites() {
    let directory = Directory::new();
    std::fs::write(
        directory.0.join("source.eod"),
        apteronotus_songs::NIGHTSHIFT_DUB,
    )
    .unwrap();
    succeeds(&directory.run(&[
        "--song",
        "nightshift-dub",
        "--seconds",
        "0.1",
        "--tail",
        "0",
        "--quiet",
    ]));
    succeeds(&directory.run(&["source.eod", "--seconds", "0.1", "--tail", "0", "--quiet"]));
    let embedded = directory.0.join("nightshift-dub.wav");
    let samples = wav_samples(&embedded);
    assert!(samples.iter().any(|sample| sample.abs() > 0.0001));
    assert_eq!(samples, wav_samples(&directory.0.join("source.wav")));
    let before = std::fs::read(&embedded).unwrap();
    let refused = directory.run(&[
        "--song",
        "nightshift-dub",
        "--seconds",
        "0.2",
        "--tail",
        "0",
    ]);
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--force"));
    assert_eq!(before, std::fs::read(&embedded).unwrap());
    succeeds(&directory.run(&[
        "--song",
        "nightshift-dub",
        "--seconds",
        "0.2",
        "--tail",
        "0",
        "--force",
        "--quiet",
    ]));
    assert_eq!(wav_samples(&embedded).len(), samples.len() * 2);
}

#[test]
fn invalid_input_is_a_diagnostic_instead_of_a_panic_or_an_output_file() {
    let directory = Directory::new();
    for arguments in [
        vec!["--song", "missing"],
        vec!["--song", "drift", "--cycles", "-9223372036854775808/-1"],
        vec!["--song", "drift", "--cycles", "1/-9223372036854775808"],
        vec!["--list-songs", "--json"],
    ] {
        let result = directory.run(&arguments);
        assert_eq!(result.status.code(), Some(1));
        let error = String::from_utf8_lossy(&result.stderr);
        assert!(error.contains("apteronotus-render:"));
        assert!(!error.contains("panicked"));
    }
    assert_eq!(std::fs::read_dir(&directory.0).unwrap().count(), 0);
}

#[test]
fn exclusive_wav_creation_claims_a_path_once_even_for_concurrent_writers() {
    use apteronotus_render::{RenderOptions, RenderSpan, render, write_wav_new};
    let directory = Directory::new();
    let path = directory.0.join("exclusive.wav");
    let program = apteronotus_lua::evaluate(
        "local v = voice {graph = function() return sine(220) * 0.1 end}; play(v, 'c4')",
    )
    .unwrap();
    let rendered = render(
        &program,
        &RenderOptions {
            span: RenderSpan::Seconds(0.01),
            tail_seconds: 0.0,
            ..Default::default()
        },
    )
    .unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let attempts: Vec<_> = (0..2)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    write_wav_new(&path, &rendered).is_ok()
                })
            })
            .collect();
        let successes = attempts
            .into_iter()
            .map(|attempt| usize::from(attempt.join().unwrap()))
            .sum::<usize>();
        assert_eq!(successes, 1);
    });
    assert_eq!(wav_samples(&path), rendered.samples);
}

#[test]
fn syntax_check_never_evaluates_the_document_or_touches_an_output() {
    let directory = Directory::new();
    let source = directory.0.join("loop.eod");
    let output = directory.0.join("loop.wav");
    std::fs::write(&source, "while true do end").unwrap();
    std::fs::write(&output, "keep this material").unwrap();
    let checked = directory.run(&["loop.eod", "--check-syntax"]);
    succeeds(&checked);
    assert!(String::from_utf8_lossy(&checked.stdout).contains("not evaluated"));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "keep this material"
    );
    std::fs::write(&source, "local x = )").unwrap();
    let refused = directory.run(&["loop.eod", "--check-syntax"]);
    assert_eq!(refused.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("line 1"));
    assert_eq!(
        std::fs::read_to_string(&output).unwrap(),
        "keep this material"
    );
    for flag in ["--json", "--list-tracks"] {
        assert!(
            !directory
                .run(&["loop.eod", "--check-syntax", flag])
                .status
                .success()
        );
    }
}
