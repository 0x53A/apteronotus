use apteronotus_live::{AudioOutput, PitchScheduler, Transport};
use apteronotus_lua::evaluate;
use std::error::Error;
use std::time::{Duration, Instant};

const SOURCE: &str = r#"
local v = voice {
  graph = function(n)
    return sine(n.hz) * n.velocity * (1 + n.duration * 0) * 0.12
  end,
}
play(v, "c4 e4 g4")
"#;

fn main() -> Result<(), Box<dyn Error>> {
    let program = evaluate(SOURCE)?;
    let track = &program.tracks[0];
    let voice = program
        .voice(track.voice)
        .expect("evaluation validated the track's VoiceId");

    let transport = Transport::new(120.0)?;
    let mut scheduler = PitchScheduler::default();
    let mut output = AudioOutput::open(voice.channels())?;
    scheduler.fill_to_seconds(
        0.25,
        &track.pattern,
        voice,
        transport,
        output.sequencer_mut(),
    )?;
    output.play()?;

    let started = Instant::now();
    let play_for = Duration::from_secs(8);
    while started.elapsed() < play_for {
        scheduler.fill_to_seconds(
            started.elapsed().as_secs_f64() + 0.2,
            &track.pattern,
            voice,
            transport,
            output.sequencer_mut(),
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
