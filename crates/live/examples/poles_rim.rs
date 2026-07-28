use apteronotus_live::{AudioOutput, PitchScheduler, Transport};
use apteronotus_pattern::mini;
use apteronotus_synth::stdlib::ring;
use apteronotus_synth::{GraphBuilder, n};
use std::error::Error;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    // The rim ignores pitch; `c4` supplies ordinary discrete onsets to the
    // current pitch-only scheduler while the future score model is unsettled.
    let pattern = mini::parse("c4(5,8)")?;
    let mut graph = GraphBuilder::new();
    let strike = graph.impulse();
    let resonator = ring(&mut graph, strike, 1_700.0, 0.028)?;
    let filtered = graph.highpass(resonator, 400.0, 0.7);
    let weighted = graph.mul(filtered, n::VELOCITY);
    let clean = graph.dcblock(weighted);
    let (left, right) = graph.pan(clean, -0.25);
    let rim = graph.out(&[left, right])?;

    let transport = Transport::new(96.0)?;
    let mut scheduler = PitchScheduler::default();
    let mut output = AudioOutput::open(rim.channels())?;
    scheduler.fill_to_seconds(0.25, &pattern, &rim, transport, output.sequencer_mut())?;
    output.play()?;

    let started = Instant::now();
    let play_for = Duration::from_secs(8);
    while started.elapsed() < play_for {
        scheduler.fill_to_seconds(
            started.elapsed().as_secs_f64() + 0.2,
            &pattern,
            &rim,
            transport,
            output.sequencer_mut(),
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
