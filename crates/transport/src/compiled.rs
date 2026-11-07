//! An allocation-free inverse of the tempo integral for sample-clock consumers.
use crate::TempoMap;
use apteronotus_pattern::Frac;

#[derive(Clone, Copy, Debug, PartialEq)]
struct Segment {
    seconds: f64,
    cycle: f64,
    bpm: f64,
    slope: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct CompiledTempo {
    scale: f64,
    segments: Vec<Segment>,
}
impl CompiledTempo {
    pub fn new(map: &TempoMap) -> Self {
        let mut cuts = vec![Frac::ZERO];
        for point in map.points() {
            if point.at > Frac::ZERO {
                cuts.push(point.at);
            }
            if let Some(over) = point.over {
                let start = point.at - over;
                if start > Frac::ZERO {
                    cuts.push(start);
                }
            }
        }
        cuts.sort();
        cuts.dedup();
        let mut segments = Vec::with_capacity(cuts.len());
        for (i, begin) in cuts.iter().copied().enumerate() {
            // Interior coordinates avoid picking the next regime at a step boundary.
            let slope = if let Some(end) = cuts.get(i + 1).copied() {
                let a = begin + (end - begin) * Frac::new(1, 3);
                let b = begin + (end - begin) * Frac::new(2, 3);
                (map.bpm_at(b) - map.bpm_at(a)) / (b - a).to_f64()
            } else {
                0.0
            };
            segments.push(Segment {
                seconds: map.cycle_to_seconds(begin),
                cycle: begin.to_f64(),
                bpm: map.bpm_at(begin),
                slope,
            });
        }
        Self {
            scale: 60.0 * map.beats_per_cycle(),
            segments,
        }
    }
    /// No iteration proportional to elapsed time, allocation or rational approximation.
    pub fn cycle_at(&self, seconds: f64) -> f64 {
        let index = self
            .segments
            .partition_point(|s| s.seconds <= seconds)
            .saturating_sub(1);
        let s = self.segments[index];
        let dt = seconds - s.seconds;
        s.cycle
            + if s.slope.abs() < 1e-12 {
                dt * s.bpm / self.scale
            } else {
                s.bpm / s.slope * (s.slope * dt / self.scale).exp_m1()
            }
    }
    pub fn storage_values(&self) -> usize {
        self.segments.len() * 4 + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::TempoPoint;
    #[test]
    fn compiled_inverse_agrees_through_ramps_and_steps() {
        let map = TempoMap::new(
            4.0,
            vec![
                TempoPoint::step(Frac::ZERO, 120.0),
                TempoPoint::ramp(Frac::from(4), 60.0, Frac::from(3)),
                TempoPoint::step(Frac::from(7), 96.0),
            ],
        )
        .unwrap();
        let clock = CompiledTempo::new(&map);
        for n in 0..1000 {
            let at = Frac::new(n, 100);
            assert!((clock.cycle_at(map.cycle_to_seconds(at)) - at.to_f64()).abs() < 1e-10);
        }
    }
}
