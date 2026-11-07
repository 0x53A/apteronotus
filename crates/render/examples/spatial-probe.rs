//! Device-free room impulse export and six-emitter block benchmark.
use apteronotus_synth::spatial::{
    Emitter, Listener, Material, RoomSpec, RoomUnit, prepare_emitter,
};
use fundsp::prelude32::{AudioUnit, BufferVec};
use std::{path::PathBuf, time::Instant};
fn cpu_seconds() -> Option<f64> {
    // Linux-only diagnostic; render timing itself remains portable.
    let text = std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
    let hz = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()?;
    let hz = std::str::from_utf8(&hz.stdout)
        .ok()?
        .trim()
        .parse::<f64>()
        .ok()?;
    Some((fields[11].parse::<f64>().ok()? + fields[12].parse::<f64>().ok()?) / hz)
}
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = PathBuf::from(
        std::env::args()
            .nth(1)
            .ok_or("provide an output directory")?,
    );
    std::fs::create_dir_all(&directory)?;
    let players = [
        ("lute", [2.5, 3.2, 1.15]),
        ("violin", [4.1, 3.8, 1.45]),
        ("flute", [5.5, 3.4, 1.5]),
        ("cello", [2.2, 4.6, 0.85]),
        ("drum", [5.6, 4.8, 0.8]),
        ("shaker", [4.9, 4.9, 1.35]),
    ];
    let spec = RoomSpec {
        size: [8., 7., 3.1],
        walls: Material::Wood,
        floor: Material::Wood,
        ceiling: Material::Plaster,
        decay: 0.58,
        listener: Listener {
            position: [4., 1.3, 1.25],
            facing: [0., 1., 0.],
        },
        emitters: players
            .iter()
            .map(|(name, position)| Emitter {
                name: name.to_string(),
                position: *position,
            })
            .collect(),
    };
    for (i, (name, _)) in players.iter().enumerate() {
        let response = prepare_emitter(&spec, i, 48000.)?;
        let path = directory.join(format!("{name}-direct.wav"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)?;
        let mut writer = hound::WavWriter::new(
            file,
            hound::WavSpec {
                channels: 2,
                sample_rate: 48000,
                bits_per_sample: 32,
                sample_format: hound::SampleFormat::Float,
            },
        )?;
        for i in 0..response.direct[0].len() {
            for ear in 0..2 {
                writer.write_sample(response.direct[ear][i])?;
            }
        }
        writer.finalize()?;
    }
    let start = Instant::now();
    let mut room = RoomUnit::new(spec, 48000.)?;
    let preparation_ms = start.elapsed().as_secs_f64() * 1000.;
    let mut input = BufferVec::new(11);
    let mut output = BufferVec::new(2);
    let mut times = Vec::with_capacity(7500);
    let cpu_start = cpu_seconds();
    for block in 0..8500 {
        for i in 0..64 {
            for ch in 0..6 {
                input.buffer_mut().set_f32(
                    ch,
                    i,
                    ((block * 64 + i) as f32 * (0.017 + ch as f32 * 0.006)).sin() * 0.05,
                );
            }
            for ch in 6..10 {
                input.buffer_mut().set_f32(ch, i, 1.);
            }
            input.buffer_mut().set_f32(10, i, 0.2);
        }
        let start = Instant::now();
        room.process(64, &input.buffer_ref(), &mut output.buffer_mut());
        if block >= 1000 {
            times.push(start.elapsed().as_secs_f64() * 1000.);
        }
    }
    let cpu_seconds = cpu_start.zip(cpu_seconds()).map(|(start, end)| end - start);
    times.sort_by(f64::total_cmp);
    let report = serde_json::json!({"emitters":6,"rate":48000,"block_frames":64,"block_budget_ms":64./48.,"preparation_ms":preparation_ms,"cpu_seconds_including_input_generation":cpu_seconds,"rendered_seconds_including_warmup":8500.*64./48000.,"mean_ms":times.iter().sum::<f64>()/times.len() as f64,"p99_ms":times[times.len()*99/100],"max_ms":times[times.len()-1]});
    let text = serde_json::to_string_pretty(&report)?;
    std::fs::write(directory.join("benchmark.json"), &text)?;
    println!("{text}");
    Ok(())
}
