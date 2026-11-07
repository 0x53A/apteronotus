//! Synthetic MIDI voice CPU benchmark without an audio device.
use apteronotus_live::{MidiVoices, PersistentRuntime, RoutedRuntime};
use apteronotus_synth::EventRouting;
use fundsp::{
    net::Net,
    prelude32::{AudioUnit, BufferVec},
};
use std::time::Instant;

fn main() {
    let source = std::env::args()
        .nth(1)
        .map(|path| std::fs::read_to_string(path).unwrap())
        .unwrap_or_else(|| include_str!("../../test-fixtures/midi-load.eod").into());
    let program = apteronotus_lua::evaluate(&source).unwrap();
    let graph = program.voice(program.midi_bindings[0].voice).unwrap();
    println!(
        "MIDI voice: {} graph nodes/key, 48 kHz, 2048-frame callbacks",
        graph.nodes.len()
    );
    for (count, bursts) in [
        (1, false),
        (8, false),
        (32, false),
        (100, false),
        (100, true),
    ] {
        let mut runtime = PersistentRuntime::new(
            &program.buses,
            &program.controls,
            program
                .runs
                .iter()
                .filter(|r| r.span.is_none())
                .map(|r| program.patch(r.patch).unwrap()),
        )
        .unwrap();
        let mut seq = runtime.sequencer();
        seq.set_sample_rate(48_000.0);
        let backend = seq.backend();
        let processor = runtime.take_processor();
        let mut net = Net::new(0, 2);
        let source = net.push(Box::new(backend));
        let effects = net.push(processor);
        for lane in 0..runtime.layout().total_channels() {
            net.connect(source, lane, effects, lane);
        }
        for lane in 0..2 {
            net.connect_output(effects, lane, lane);
        }
        net.set_sample_rate(48_000.0);
        net.allocate();
        let mut keys = MidiVoices::default();
        let before = Instant::now();
        for key in 24..24 + count {
            keys.note_on(
                0,
                key,
                100,
                0.0,
                u64::from(key),
                graph,
                &EventRouting::new(),
                &mut seq,
                RoutedRuntime::new(runtime.layout(), runtime.controls()),
            )
            .unwrap();
        }
        let setup_ms = before.elapsed().as_secs_f64() * 1000.0;
        let input = BufferVec::new(0);
        let mut output = BufferVec::new(2);
        let mut times = Vec::new();
        for callback in 0..if bursts { 24 } else { 48 } {
            let now = callback as f64 * 2048.0 / 48000.0;
            keys.prune(now);
            if bursts && callback % 4 == 2 {
                for key in 24..124 {
                    keys.note_off(0, key, now, &mut seq);
                }
            }
            if bursts && callback > 0 && callback % 4 == 0 {
                let started = Instant::now();
                for key in 24..124 {
                    keys.note_on(
                        0,
                        key,
                        100,
                        now,
                        callback * 128 + u64::from(key),
                        graph,
                        &EventRouting::new(),
                        &mut seq,
                        RoutedRuntime::new(runtime.layout(), runtime.controls()),
                    )
                    .unwrap();
                }
                println!(
                    "    retrigger construction {:.2} ms",
                    started.elapsed().as_secs_f64() * 1000.0
                );
            }
            let started = Instant::now();
            for _ in 0..32 {
                net.process(64, &input.buffer_ref(), &mut output.buffer_mut());
            }
            times.push(started.elapsed().as_secs_f64() * 1000.0);
        }
        println!(
            "    early {:.2} ms, late {:.2} ms",
            times[..8].iter().sum::<f64>() / 8.0,
            times[times.len() - 8..].iter().sum::<f64>() / 8.0
        );
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        if bursts {
            println!("six 100-key bursts, 85ms gates/rests:");
        }
        println!(
            "{count:3} keys: construct {setup_ms:8.2} ms; render mean {mean:7.2} ms, p95 {:7.2} ms, max {:7.2} ms; {:.1}% realtime",
            times[times.len() * 95 / 100],
            times[times.len() - 1],
            mean / (2048.0 / 48.0) * 100.0
        );
    }
}
