use apteronotus_live::{AudioOutput, PitchScheduler, Transport};
use apteronotus_pattern::mini;
use apteronotus_synth::{Adsr, GraphBuilder, n};
use std::error::Error;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let pattern = mini::parse("c4 e4 g4 c5")?;
    let mut graph = GraphBuilder::new();
    let oscillator = graph.sine(n::HZ);
    let filtered = graph.lowpass(oscillator, 3_000.0, 0.7);
    let envelope = graph.adsr(Adsr::new(0.005, 0.08, 0.55, 0.18));
    let shaped = graph.mul(filtered, envelope);
    let scaled = graph.mul(shaped, 0.16);
    let voice = graph.out_panned(scaled)?;

    let transport = Transport::new(96.0)?;
    let mut scheduler = PitchScheduler::default();
    let mut output = AudioOutput::open(voice.channels())?;

    // Prime the bounded frontend/backend channel before the audio clock starts.
    scheduler.fill_to_seconds(0.25, &pattern, &voice, transport, output.sequencer_mut())?;
    output.play()?;

    let started = Instant::now();
    let play_for = Duration::from_secs(8);
    let lookahead = 0.2;
    while started.elapsed() < play_for {
        scheduler.fill_to_seconds(
            started.elapsed().as_secs_f64() + lookahead,
            &pattern,
            &voice,
            transport,
            output.sequencer_mut(),
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
