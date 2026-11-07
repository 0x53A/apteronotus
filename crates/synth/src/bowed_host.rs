//! Bounded device-neutral delivery for retained violin actions.
use crate::bowed::BowedConfig;
use crate::bowed_gesture::{
    BowAction, BowCommand, BowedPerformance, MAX_PER_BLOCK, MAX_PER_SAMPLE,
};
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, SignalFrame};
use std::collections::VecDeque;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

/// One conversion shared by score preparation and live delivery.
pub fn sample_at(seconds: f64, rate: f64) -> Result<u64, &'static str> {
    let sample = (seconds * rate).ceil();
    if !seconds.is_finite()
        || seconds < 0.0
        || !rate.is_finite()
        || rate <= 0.0
        || !sample.is_finite()
        || sample >= u64::MAX as f64
    {
        Err("gesture time outside representable sample coordinates")
    } else {
        Ok(sample as u64)
    }
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GestureCoordinate {
    pub ordinal: u64,
    pub seconds: f64,
    pub duration: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct BowedScore {
    pub config: BowedConfig,
    /// Live mode owns speed, pressure multiplier and contact position through graph inputs.
    pub live: bool,
    pub clock: Option<apteronotus_transport::CompiledTempo>,
    pub opens: Vec<f64>,
    /// Exact sample coordinates at reference_rate. Device setup converts once.
    pub commands: Vec<BowCommand>,
    /// Authored seconds are retained until the actual device-rate conversion.
    pub coordinates: Vec<GestureCoordinate>,
    pub reference_rate: f64,
}
impl BowedScore {
    pub fn delivery_cost(&self) -> (usize, usize) {
        if self.live {
            return (MAX_PER_SAMPLE, MAX_PER_BLOCK);
        }
        let mut times: Vec<_> = self.commands.iter().map(|c| c.sample).collect();
        times.sort_unstable();
        let mut sample = 0;
        let mut block = 0;
        for (i, at) in times.iter().copied().enumerate() {
            sample = sample.max(times[i..].iter().take_while(|t| **t == at).count());
            block = block.max(
                times[i..]
                    .iter()
                    .take_while(|t| **t < at.saturating_add(64))
                    .count(),
            );
        }
        (sample, block)
    }
    pub fn prepare(&self, rate: f64) -> Result<BowedPerformance, &'static str> {
        if self.live && !self.commands.is_empty() {
            return Err("live and scored gesture ownership cannot be combined");
        }
        if !self.reference_rate.is_finite() || self.reference_rate < 24_000.0 {
            return Err("invalid gesture reference rate");
        }
        if self.coordinates.len() > crate::bowed_gesture::MAX_COMMANDS {
            return Err("gesture coordinate storage limit");
        }
        if !self.coordinates.is_empty()
            && (self.coordinates.len() != self.commands.len()
                || self.commands.iter().any(|c| {
                    self.coordinates
                        .iter()
                        .filter(|t| t.ordinal == c.ordinal)
                        .count()
                        != 1
                }))
        {
            return Err("gesture coordinates must correspond one-to-one with command identities");
        }
        let mut seconds: Vec<_> = self
            .commands
            .iter()
            .map(|c| {
                self.coordinates
                    .iter()
                    .find(|t| t.ordinal == c.ordinal)
                    .map_or(c.sample as f64 / self.reference_rate, |t| t.seconds)
            })
            .collect();
        seconds.sort_by(f64::total_cmp);
        for (i, time) in seconds.iter().copied().enumerate() {
            if seconds[i..]
                .iter()
                .take_while(|t| **t - time <= 1.0 / 24000.0)
                .count()
                > MAX_PER_SAMPLE
                || seconds[i..]
                    .iter()
                    .take_while(|t| **t - time <= 64.0 / 24000.0)
                    .count()
                    > MAX_PER_BLOCK
            {
                return Err("gesture density exceeds the supported rate domain");
            }
        }
        let commands = self
            .commands
            .iter()
            .map(|c| {
                let authored = self
                    .coordinates
                    .iter()
                    .find(|time| time.ordinal == c.ordinal);
                let seconds = authored.map_or(c.sample as f64 / self.reference_rate, |t| t.seconds);
                let start = sample_at(seconds, rate)?;
                let convert = |n: u64| {
                    let duration = authored.map_or(n as f64 / self.reference_rate, |t| t.duration);
                    if !duration.is_finite() || duration < 0.0 {
                        return Err("invalid gesture duration");
                    }
                    sample_at(seconds + duration, rate)?
                        .checked_sub(start)
                        .ok_or("invalid gesture endpoint")
                };
                let mut action = c.action;
                match &mut action {
                    BowAction::Bow { over, .. }
                    | BowAction::Lift { over }
                    | BowAction::Stop { over, .. }
                    | BowAction::ReleaseFinger { over, .. }
                    | BowAction::Damp { over, .. }
                    | BowAction::Vibrato { over, .. }
                    | BowAction::Expression { over, .. } => *over = convert(*over)?,
                }
                Ok(BowCommand {
                    sample: start,
                    ordinal: c.ordinal,
                    action,
                    beats: c.beats,
                })
            })
            .collect::<Result<Vec<_>, &'static str>>()?;
        BowedPerformance::new_clocked(self.config, &self.opens, commands, rate, self.clock.clone())
    }
}

#[derive(Clone, Copy, Default)]
struct Packet {
    at: Option<f64>,
    epoch: u64,
    actions: [Option<BowAction>; 4],
}
struct Admission {
    epoch: u64,
    last: f64,
    window: VecDeque<(u64, usize)>,
}
struct Shared {
    live: bool,
    rate: AtomicU64,
    queue: thingbuf::ThingBuf<Packet>,
    admission: Mutex<Admission>,
    epoch: AtomicU64,
    panic: AtomicBool,
    frontier: AtomicU64,
    rejected: AtomicU64,
    late: AtomicU64,
    guards: AtomicU64,
}
#[derive(Clone)]
pub struct BowedHandle {
    shared: Arc<Shared>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct BowedHostDiagnostics {
    pub rejected: u64,
    pub late: u64,
    pub guards: u64,
}
impl BowedHandle {
    pub(crate) fn new(live: bool) -> Self {
        // Each packet has at most four commands: storage never exceeds 4096.
        Self {
            shared: Arc::new(Shared {
                queue: thingbuf::ThingBuf::new(1024),
                live,
                rate: AtomicU64::new(44_100.0_f64.to_bits()),
                admission: Mutex::new(Admission {
                    epoch: 0,
                    last: 0.0,
                    window: VecDeque::with_capacity(1024),
                }),
                epoch: AtomicU64::new(0),
                panic: AtomicBool::new(false),
                frontier: AtomicU64::new(0),
                rejected: AtomicU64::new(0),
                late: AtomicU64::new(0),
                guards: AtomicU64::new(0),
            }),
        }
    }
    pub fn frontier_seconds(&self) -> f64 {
        f64::from_bits(self.shared.frontier.load(Ordering::Acquire))
    }
    pub fn diagnostics(&self) -> BowedHostDiagnostics {
        BowedHostDiagnostics {
            rejected: self.shared.rejected.load(Ordering::Relaxed),
            late: self.shared.late.load(Ordering::Relaxed),
            guards: self.shared.guards.load(Ordering::Relaxed),
        }
    }
    /// Queue one atomic live batch. None means the next sample seen by audio;
    /// future absolute seconds remain future until the renderer reaches them.
    pub fn send(&self, at: Option<f64>, actions: &[BowAction]) -> Result<(), &'static str> {
        if !self.shared.live
            || actions.is_empty()
            || actions.len() > 4
            || at.is_some_and(|v| !v.is_finite() || v < self.frontier_seconds())
        {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
            return Err("invalid/late bowed live batch");
        }
        let epoch = self.shared.epoch.load(Ordering::Acquire);
        let mut admission = self
            .shared
            .admission
            .lock()
            .map_err(|_| "bowed admission unavailable")?;
        let effective = at.unwrap_or_else(|| self.frontier_seconds());
        if admission.epoch == epoch && effective < admission.last {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
            return Err(
                "bowed live timestamps must be nondecreasing; immediate commands cannot overtake future commands",
            );
        }
        if admission.epoch != epoch {
            admission.window.clear();
        }
        let rate = f64::from_bits(self.shared.rate.load(Ordering::Acquire));
        let sample = sample_at(effective, rate)?;
        while admission
            .window
            .front()
            .is_some_and(|(s, _)| s.saturating_add(64) <= sample)
        {
            admission.window.pop_front();
        }
        let same: usize = admission
            .window
            .iter()
            .filter(|(s, _)| *s == sample)
            .map(|(_, n)| *n)
            .sum();
        let block: usize = admission.window.iter().map(|(_, n)| *n).sum();
        if same + actions.len() > MAX_PER_SAMPLE || block + actions.len() > MAX_PER_BLOCK {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
            return Err("bowed live delivery density limit exceeded");
        }
        let mut packet = Packet {
            at,
            epoch,
            ..Default::default()
        };
        for (slot, action) in packet.actions.iter_mut().zip(actions) {
            *slot = Some(*action);
        }
        self.shared.queue.push(packet).map_err(|_| {
            self.shared.rejected.fetch_add(1, Ordering::Relaxed);
            "bowed live queue full"
        })?;
        admission.epoch = epoch;
        admission.last = effective;
        admission.window.push_back((sample, actions.len()));
        Ok(())
    }
    /// Reserved emergency path that does not need an ordinary queue slot.
    pub fn panic_detach(&self) {
        self.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.shared.panic.store(true, Ordering::Release);
    }
}

#[derive(Clone)]
pub(crate) struct BowedInstrumentUnit {
    score: BowedScore,
    performance: BowedPerformance,
    handle: BowedHandle,
    pending: Option<Packet>,
    rate: f64,
    block_work: usize,
    faulted: bool,
    origin: f64,
}
impl BowedInstrumentUnit {
    pub(crate) fn new(
        score: BowedScore,
        handle: BowedHandle,
        origin: f64,
    ) -> Result<Self, &'static str> {
        let rate = 44_100.0;
        let mut performance = score.prepare(rate)?;
        performance.preroll_to(sample_at(origin, rate)?, score.live);
        handle
            .shared
            .frontier
            .store(origin.to_bits(), Ordering::Release);
        Ok(Self {
            score,
            performance,
            handle,
            pending: None,
            rate,
            block_work: 0,
            faulted: false,
            origin,
        })
    }
    fn frame(&mut self, input: &[f32]) -> [f32; 2] {
        if self.faulted {
            return [0.0; 2];
        }
        let sample = self.performance.sample_position();
        if sample.is_multiple_of(64) {
            self.block_work = 0;
        }

        if self.handle.shared.panic.swap(false, Ordering::AcqRel) {
            let _ = self.performance.apply_now(BowAction::Lift { over: 0 });
        }
        let mut work = self.performance.commands_due();
        self.block_work += work;
        while work < MAX_PER_SAMPLE {
            let packet = match self
                .pending
                .take()
                .or_else(|| self.handle.shared.queue.pop())
            {
                Some(p) => p,
                None => break,
            };
            let current = packet.epoch == self.handle.shared.epoch.load(Ordering::Acquire);
            let coordinate = packet
                .at
                .map(|at| sample_at(at, self.rate).expect("admitted finite live time"));
            if current && coordinate.is_some_and(|at| at > sample) {
                self.pending = Some(packet);
                break;
            }
            let count = packet.actions.iter().flatten().count();
            if work + count > MAX_PER_SAMPLE || self.block_work + count > MAX_PER_BLOCK {
                self.pending = Some(packet);
                break;
            }
            work += count;
            self.block_work += count;
            if !current {
                continue;
            }
            if coordinate.is_some_and(|at| at < sample) {
                self.handle.shared.late.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            // Each packet is applied atomically, including rollback on invalid fields.
            if self.performance.apply_batch(&packet.actions).is_err() {
                self.handle.shared.rejected.fetch_add(1, Ordering::Relaxed);
            }
        }
        let result = if self.score.live {
            self.performance
                .next_live_frame([input[0] as f64, input[1] as f64, input[2] as f64])
        } else {
            self.performance.next_frame()
        };
        if sample.is_multiple_of(64) {
            self.handle.shared.guards.store(
                self.performance.diagnostics().emergency_resets,
                Ordering::Relaxed,
            );
        }
        self.handle.shared.frontier.store(
            ((sample + 1) as f64 / self.rate).to_bits(),
            Ordering::Release,
        );
        result
    }
}
impl AudioUnit for BowedInstrumentUnit {
    fn reset(&mut self) {
        self.performance.reset();
        self.pending = None;
        self.block_work = 0;
        self.handle.shared.epoch.fetch_add(1, Ordering::AcqRel);
        self.handle.shared.panic.store(false, Ordering::Release);
        self.handle
            .shared
            .frontier
            .store(0.0_f64.to_bits(), Ordering::Release);
    }
    fn set_sample_rate(&mut self, rate: f64) {
        self.handle
            .shared
            .rate
            .store(rate.to_bits(), Ordering::Release);
        if self.rate != rate {
            if let Ok(mut p) = self.score.prepare(rate) {
                p.preroll_to((self.origin * rate).ceil() as u64, self.score.live);
                self.performance = p;
                self.faulted = false;
                self.rate = rate;
            } else {
                self.handle.shared.rejected.fetch_add(1, Ordering::Relaxed);
                self.faulted = true;
            }
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        output.copy_from_slice(&self.frame(input));
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        for i in 0..size {
            let y = self.frame(&[input.at_f32(0, i), input.at_f32(1, i), input.at_f32(2, i)]);
            output.set_f32(0, i, y[0]);
            output.set_f32(1, i, y[1]);
        }
    }
    fn inputs(&self) -> usize {
        3
    }
    fn outputs(&self) -> usize {
        2
    }
    fn route(&mut self, input: &SignalFrame, f: f64) -> SignalFrame {
        self.performance.route(input, f)
    }
    fn get_id(&self) -> u64 {
        0x4150_5442_484f_5354
    }
    fn footprint(&self) -> usize {
        self.performance.footprint() + 1024 * std::mem::size_of::<Packet>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn unit() -> BowedInstrumentUnit {
        let score = BowedScore {
            config: crate::bowed_gesture::c2_config(),
            live: true,
            clock: None,
            opens: vec![293.6648],
            commands: vec![],
            coordinates: vec![],
            reference_rate: 48_000.0,
        };
        BowedInstrumentUnit::new(score, BowedHandle::new(true), 0.0).unwrap()
    }
    fn bow() -> BowAction {
        BowAction::Bow {
            string: Some(0),
            speed: Some(0.4),
            force: Some(1.0),
            position: None,
            over: 0,
        }
    }
    #[test]
    fn future_commands_do_not_write_early_and_panic_invalidates_pending_bows() {
        let mut unit = unit();
        unit.handle.send(Some(1.0), &[bow()]).unwrap();
        assert!(unit.handle.send(Some(0.5), &[bow()]).is_err());
        for _ in 0..100 {
            assert_eq!(unit.frame(&[0.4, 0.55, 0.12]), [0.0, 0.0]);
        }
        unit.handle.panic_detach();
        for _ in 0..50000 {
            assert_eq!(unit.frame(&[0.4, 0.55, 0.12]), [0.0, 0.0]);
        }
        assert_eq!(
            unit.performance.state().contact,
            crate::bowed_gesture::BowContact::Detached
        );
    }
    #[test]
    fn admitted_block_budget_does_not_charge_peeks_at_future_actions() {
        let mut u = unit();
        for sample in (0..64).step_by(8) {
            let time = if sample == 0 {
                0.0
            } else {
                (sample as f64 - 0.1) / 44100.0
            };
            for _ in 0..32 {
                u.handle.send(Some(time), &[bow(); 4]).unwrap();
            }
        }
        u.handle
            .send(Some(63.9 / 44100.0), &[BowAction::Lift { over: 0 }])
            .unwrap();
        for _ in 0..65 {
            u.frame(&[0.4, 0.55, 0.12]);
        }
        assert_eq!(u.handle.diagnostics().late, 0);
        assert_eq!(u.handle.diagnostics().rejected, 0);
        assert_eq!(
            u.performance.state().contact,
            crate::bowed_gesture::BowContact::Detached
        );
    }
    #[test]
    fn late_queue_work_and_overflow_are_bounded_and_diagnosed() {
        let mut unit = unit();
        for sample in 0..1024 {
            unit.handle
                .send(Some(sample as f64 / 44_100.0), &[bow(); 4])
                .unwrap();
        }
        assert!(unit.handle.send(Some(1024.0 / 44_100.0), &[bow()]).is_err());
        unit.frame(&[0.4, 0.55, 0.12]);
        assert_eq!(
            unit.performance.state().contact,
            crate::bowed_gesture::BowContact::Engaged
        );
        unit.frame(&[0.4, 0.55, 0.12]);
        assert_eq!(unit.handle.diagnostics().late, 0);
        // Releasing cannot depend on finding an ordinary slot.
        unit.handle.panic_detach();
        unit.frame(&[0.4, 0.55, 0.12]);
        assert_eq!(
            unit.performance.state().contact,
            crate::bowed_gesture::BowContact::Detached
        );
    }
}
