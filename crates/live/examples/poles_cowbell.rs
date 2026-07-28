use apteronotus_live::{AudioOutput, PitchScheduler, Transport};
use apteronotus_pattern::mini;
use apteronotus_synth::stdlib::ring;
use apteronotus_synth::{GraphBuilder, ParamSpec, ShapeKind};
use std::error::Error;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    // The current scheduler still wants pitches; the cowbell ignores `c4`.
    // This is the rhythm from poles.eod, with its default 180 ms ring.
    let pattern = mini::parse("~ c4 ~ ~ c4 ~ ~ ~")?;
    let mut graph = GraphBuilder::new();
    let decay = graph.param(ParamSpec::new("ring", 0.02, 2.0, 0.18).with_unit("s"));
    let strike = graph.impulse();
    let low = ring(&mut graph, strike, 587.0, decay)?;
    let short_decay = graph.mul(decay, 0.85);
    let high = ring(&mut graph, strike, 845.0, short_decay)?;
    let high = graph.mul(high, 0.7);
    let modes = graph.add(low, high);
    // Calibrate the fundsp SVF impulse level into the nonlinear range.
    let modes = graph.mul(modes, 7.0);
    let driven = graph.shape(modes, ShapeKind::Tanh, 1.3);
    let clean = graph.dcblock(driven);
    let cowbell = graph.out_panned(clean)?;

    let transport = Transport::new(96.0)?;
    let mut scheduler = PitchScheduler::default();
    let mut output = AudioOutput::open(cowbell.channels())?;
    scheduler.fill_to_seconds(0.25, &pattern, &cowbell, transport, output.sequencer_mut())?;
    output.play()?;

    let started = Instant::now();
    let play_for = Duration::from_secs(8);
    while started.elapsed() < play_for {
        scheduler.fill_to_seconds(
            started.elapsed().as_secs_f64() + 0.2,
            &pattern,
            &cowbell,
            transport,
            output.sequencer_mut(),
        )?;
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
