//! `apteronotus-render` — evaluate a song and write it to an audio file.
//!
//! The point of this binary is not export, it is measurement. A rendered file
//! is the only way anything outside the process can inspect what the engine
//! produced: a spectral analysis, a regression against a previous render, or a
//! comparison against the reference recording a song is being built to
//! resemble. Playback already exists; being able to look at the result did
//! not.

use apteronotus_lua::evaluate;
use apteronotus_pattern::Frac;
use apteronotus_render::{RenderOptions, RenderSpan, SampleFormat, render, write_wav};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "\
usage: apteronotus-render <song.eod> [options]

  -o, --output <file.wav>  where to write        (default: the song's name, beside it)
      --seconds <n>        scheduled duration    (default: 30)
      --cycles <n[/d]>     scheduled duration in cycles, through the song's tempo map
      --tail <n>           rendered past the end (default: 2)
      --sample-rate <n>    in hertz              (default: 48000)
      --stems              write every routed bus, not just the main channels
      --pcm16              16-bit PCM instead of 32-bit float
  -f, --force              overwrite an existing output file
  -q, --quiet              print nothing on success
  -h, --help               this text
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let invocation = match parse(&arguments) {
        Ok(Some(invocation)) => invocation,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("apteronotus-render: {message}\n\n{USAGE}");
            return ExitCode::FAILURE;
        }
    };
    match run(&invocation) {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("apteronotus-render: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Invocation {
    input: PathBuf,
    output: PathBuf,
    options: RenderOptions,
    force: bool,
    quiet: bool,
}

fn run(invocation: &Invocation) -> Result<(), String> {
    if invocation.output.exists() && !invocation.force {
        return Err(format!(
            "{} already exists; pass --force to overwrite it",
            invocation.output.display()
        ));
    }

    let source = std::fs::read_to_string(&invocation.input)
        .map_err(|error| format!("cannot read {}: {error}", invocation.input.display()))?;
    let program = evaluate(&source).map_err(|error| format!("{error}"))?;
    let rendered = render(&program, &invocation.options).map_err(|error| format!("{error}"))?;
    write_wav(&invocation.output, &rendered).map_err(|error| format!("{error}"))?;

    // Silence and clipping are both worth interrupting for even in quiet mode:
    // one means the render proved nothing, the other means it is not a
    // faithful measurement of the mix.
    if rendered.peak == 0.0 {
        eprintln!(
            "apteronotus-render: warning: {} is entirely silent",
            invocation.output.display()
        );
    }
    if rendered.clipped > 0 {
        eprintln!(
            "apteronotus-render: warning: {} samples exceed full scale (peak {:+.1} dBFS)",
            rendered.clipped,
            rendered.peak_decibels().unwrap_or(0.0)
        );
    }
    if invocation.quiet {
        return Ok(());
    }

    let peak = match rendered.peak_decibels() {
        Some(decibels) => format!("{decibels:+.1} dBFS"),
        None => "silent".to_string(),
    };
    println!(
        "{} → {}",
        invocation.input.display(),
        invocation.output.display()
    );
    println!(
        "  {:.2} s scheduled + {:.2} s tail, {} Hz, {} ch, {}",
        rendered.scheduled_seconds,
        rendered.duration_seconds() - rendered.scheduled_seconds,
        rendered.sample_rate.round(),
        rendered.channels,
        match rendered.format {
            SampleFormat::Float32 => "float32",
            SampleFormat::Pcm16 => "pcm16",
        }
    );
    println!("  {} voices, peak {peak}", rendered.voices);
    Ok(())
}

fn parse(arguments: &[String]) -> Result<Option<Invocation>, String> {
    let mut input: Option<PathBuf> = None;
    let mut output: Option<PathBuf> = None;
    let mut options = RenderOptions::default();
    let mut span: Option<RenderSpan> = None;
    let mut force = false;
    let mut quiet = false;

    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        let mut value = |name: &str| -> Result<String, String> {
            rest.next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match argument.as_str() {
            "-h" | "--help" => return Ok(None),
            "-f" | "--force" => force = true,
            "-q" | "--quiet" => quiet = true,
            "--stems" => options.stems = true,
            "--pcm16" => options.format = SampleFormat::Pcm16,
            "-o" | "--output" => output = Some(PathBuf::from(value("--output")?)),
            "--seconds" => span = Some(RenderSpan::Seconds(number(&value("--seconds")?)?)),
            "--cycles" => span = Some(RenderSpan::Cycles(cycles(&value("--cycles")?)?)),
            "--tail" => options.tail_seconds = number(&value("--tail")?)?,
            "--sample-rate" => options.sample_rate = number(&value("--sample-rate")?)?,
            other if other.starts_with('-') && other != "-" => {
                return Err(format!("unknown option {other}"));
            }
            path => {
                if input.replace(PathBuf::from(path)).is_some() {
                    return Err("only one song can be rendered at a time".into());
                }
            }
        }
    }

    let input = input.ok_or("no song given")?;
    if let Some(span) = span {
        options.span = span;
    }
    let output = output.unwrap_or_else(|| default_output(&input));
    Ok(Some(Invocation {
        input,
        output,
        options,
        force,
        quiet,
    }))
}

/// The song's own name with a `.wav` extension, in the song's own directory.
fn default_output(input: &Path) -> PathBuf {
    let mut output = input.to_path_buf();
    output.set_extension("wav");
    output
}

fn number(text: &str) -> Result<f64, String> {
    text.parse::<f64>()
        .map_err(|_| format!("{text:?} is not a number"))
}

/// `16` or `3/2`. Cycle counts are rational for the same reason every other
/// time in the system is: a third of a cycle has to stay a third.
fn cycles(text: &str) -> Result<Frac, String> {
    let integer = |part: &str| {
        part.trim()
            .parse::<i64>()
            .map_err(|_| format!("{text:?} is not a cycle count"))
    };
    match text.split_once('/') {
        Some((numerator, denominator)) => {
            let denominator = integer(denominator)?;
            if denominator == 0 {
                return Err(format!("{text:?} has a zero denominator"));
            }
            Ok(Frac::new(integer(numerator)?, denominator))
        }
        None => Ok(Frac::new(integer(text)?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn the_output_defaults_to_the_song_beside_itself() {
        let invocation = parse(&arguments("songs/techno.eod"))
            .unwrap()
            .expect("a song is not a help request");
        assert_eq!(invocation.output, PathBuf::from("songs/techno.wav"));
        assert!(!invocation.force);
    }

    #[test]
    fn a_later_duration_flag_replaces_an_earlier_one() {
        let invocation = parse(&arguments("s.eod --seconds 4 --cycles 8"))
            .unwrap()
            .unwrap();
        assert_eq!(invocation.options.span, RenderSpan::Cycles(Frac::new(8, 1)));
    }

    #[test]
    fn cycles_stay_rational() {
        assert_eq!(cycles("3/2").unwrap(), Frac::new(3, 2));
        assert_eq!(cycles("16").unwrap(), Frac::new(16, 1));
        assert!(cycles("3/0").is_err());
        assert!(cycles("half").is_err());
    }

    #[test]
    fn options_are_carried_through() {
        let invocation = parse(&arguments(
            "s.eod -o out.wav --tail 0.5 --sample-rate 44100 --stems --pcm16 -f -q",
        ))
        .unwrap()
        .unwrap();
        assert_eq!(invocation.output, PathBuf::from("out.wav"));
        assert_eq!(invocation.options.tail_seconds, 0.5);
        assert_eq!(invocation.options.sample_rate, 44_100.0);
        assert!(invocation.options.stems);
        assert_eq!(invocation.options.format, SampleFormat::Pcm16);
        assert!(invocation.force);
        assert!(invocation.quiet);
    }

    #[test]
    fn a_missing_value_is_an_error_rather_than_a_silent_default() {
        assert!(parse(&arguments("s.eod --seconds")).is_err());
        assert!(parse(&arguments("s.eod --unknown")).is_err());
        assert!(parse(&arguments("one.eod two.eod")).is_err());
        assert!(parse(&arguments("")).is_err());
    }

    #[test]
    fn help_is_not_an_invocation() {
        assert!(parse(&arguments("--help")).unwrap().is_none());
    }
}
