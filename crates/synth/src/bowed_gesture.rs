//! Owned sample-timed actions over retained bowed strings. Preparing a plan
//! never writes a live scalar; only the sample executor changes applied state.
use crate::bowed::{BowedBody, BowedConfig, BowedControls, BowedModel, BowedString};
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, Signal, SignalFrame};

pub const MAX_COMMANDS: usize = 4096;
pub const MAX_PER_SAMPLE: usize = 128;
pub const MAX_PER_BLOCK: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum BowAction {
    Bow {
        string: Option<u8>,
        speed: Option<f64>,
        force: Option<f64>,
        position: Option<f64>,
        over: u64,
    },
    Lift {
        over: u64,
    },
    Stop {
        string: u8,
        hz: f64,
        over: u64,
    },
    ReleaseFinger {
        string: u8,
        over: u64,
    },
    Damp {
        string: u8,
        amount: f64,
        over: u64,
    },
    Vibrato {
        string: u8,
        cents: f64,
        rate: f64,
        /// Fade depth from its value at the command sample. Rate changes
        /// immediately with continuous phase; it is not a frequency ramp.
        over: u64,
    },
    Expression {
        amount: f64,
        over: u64,
    },
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BowCommand {
    pub sample: u64,
    pub ordinal: u64,
    pub action: BowAction,
    pub beats: Option<BeatRamp>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BeatRamp {
    pub begin: f64,
    pub end: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BowContact {
    Detached,
    Engaged,
    Lifting,
}

#[derive(Clone, Copy, Debug)]
struct Lane {
    from: f64,
    to: f64,
    begin: u64,
    end: u64,
    beats: Option<BeatRamp>,
}
impl Lane {
    fn fixed(value: f64) -> Self {
        Self {
            from: value,
            to: value,
            begin: 0,
            end: 0,
            beats: None,
        }
    }
    fn at(self, sample: u64, cycle: f64) -> f64 {
        if sample >= self.end {
            self.to
        } else if sample <= self.begin {
            self.from
        } else {
            let progress = if let Some(beats) = self.beats {
                ((cycle - beats.begin) / (beats.end - beats.begin)).clamp(0.0, 1.0)
            } else {
                (sample - self.begin) as f64 / (self.end - self.begin) as f64
            };
            self.from + (self.to - self.from) * progress
        }
    }
    fn replace(
        &mut self,
        target: f64,
        sample: u64,
        over: u64,
        beats: Option<BeatRamp>,
        cycle: f64,
    ) -> Result<(), &'static str> {
        let end = sample
            .checked_add(over)
            .ok_or("gesture duration overflow")?;
        *self = Self {
            from: self.at(sample, cycle),
            to: target,
            begin: sample,
            end,
            beats,
        };
        Ok(())
    }
    fn remaining_bounds(self, sample: u64, cycle: f64) -> (f64, f64) {
        let now = self.at(sample, cycle);
        (now.min(self.to), now.max(self.to))
    }
}
#[derive(Clone, Copy, Debug)]
struct Finger {
    open_hz: f64,
    pitch: Lane,
    stopped: bool,
    damping: Lane,
    depth: Lane,
    rate: f64,
    phase: f64,
    phase_sample: u64,
    bow_position: f64,
}
impl Finger {
    fn phase_at(self, sample: u64, rate: f64) -> f64 {
        self.phase + std::f64::consts::TAU * self.rate * (sample - self.phase_sample) as f64 / rate
    }
    fn hz(self, sample: u64, rate: f64, cycle: f64) -> f64 {
        let offset = if self.stopped {
            self.phase_at(sample, rate).sin() * self.depth.at(sample, cycle) / 100.0
        } else {
            0.0
        };
        440.0 * 2.0_f64.powf((self.pitch.at(sample, cycle) + offset) / 12.0)
    }
}
fn pitch(hz: f64) -> f64 {
    12.0 * (hz / 440.0).log2()
}

#[derive(Clone, Debug)]
pub struct BowState {
    pub contact: BowContact,
    pub selected: Option<u8>,
    speed: Lane,
    force: Lane,
    position: Lane,
    expression: Lane,
    fingers: Vec<Finger>,
    pending_detach: Option<u64>,
    cycle: f64,
}
impl BowState {
    fn new(opens: &[f64]) -> Self {
        Self {
            contact: BowContact::Detached,
            selected: None,
            speed: Lane::fixed(0.0),
            force: Lane::fixed(0.0),
            position: Lane::fixed(0.12),
            expression: Lane::fixed(1.0),
            pending_detach: None,
            cycle: 0.0,
            fingers: opens
                .iter()
                .map(|hz| Finger {
                    open_hz: *hz,
                    pitch: Lane::fixed(pitch(*hz)),
                    stopped: false,
                    damping: Lane::fixed(0.0),
                    depth: Lane::fixed(0.0),
                    rate: 5.0,
                    phase: 0.0,
                    phase_sample: 0,
                    bow_position: 0.12,
                })
                .collect(),
        }
    }
    fn advance(&mut self, sample: u64, cycle: f64) {
        self.cycle = cycle;
        if let Some(index) = self.selected {
            self.fingers[index as usize].bow_position = self.position.at(sample, cycle);
        }
        if self.pending_detach.is_some_and(|end| end <= sample) {
            self.force = Lane::fixed(0.0);
            self.contact = BowContact::Detached;
            self.pending_detach = None;
        }
    }
    fn finger(&mut self, string: u8) -> Result<&mut Finger, &'static str> {
        self.fingers
            .get_mut(string as usize)
            .ok_or("unknown bowed string")
    }
    fn apply(&mut self, cmd: BowCommand, rate: f64, cycle: f64) -> Result<(), &'static str> {
        self.advance(cmd.sample, cycle);
        let at = cmd.sample;
        let beats = cmd.beats;
        if beats.is_some_and(|b| !b.begin.is_finite() || !b.end.is_finite() || b.end <= b.begin) {
            return Err("invalid beat ramp");
        }
        match cmd.action {
            BowAction::Bow {
                string,
                speed,
                force,
                position,
                over,
            } => {
                if let Some(s) = string {
                    if s as usize >= self.fingers.len() {
                        return Err("unknown bowed string");
                    }
                    if self.contact != BowContact::Detached && self.selected != Some(s) {
                        return Err("engaged string crossing is unsupported");
                    }
                    self.selected = Some(s);
                }
                if self.selected.is_none() {
                    return Err("first bow requires a selected string");
                }
                if let Some(v) = speed {
                    if !v.is_finite() || !(-1.0..=1.0).contains(&v) {
                        return Err("bow speed outside -1..1");
                    }
                    self.speed.replace(v, at, over, beats, cycle)?;
                }
                if let Some(v) = position {
                    if !v.is_finite() || !(0.02..=0.45).contains(&v) {
                        return Err("bow position outside supported range");
                    }
                    self.position.replace(v, at, over, beats, cycle)?;
                }
                if let Some(v) = force {
                    if !v.is_finite() || !(0.0..=1.0).contains(&v) {
                        return Err("bow force outside 0..1");
                    }
                    self.force.replace(v, at, over, beats, cycle)?;
                    if self.contact == BowContact::Lifting || v > 0.0 {
                        self.contact = BowContact::Engaged;
                        self.pending_detach = None;
                    }
                }
            }
            BowAction::Lift { over } => {
                if self.contact != BowContact::Detached {
                    self.force.replace(0.0, at, over, beats, cycle)?;
                    self.pending_detach =
                        Some(at.checked_add(over).ok_or("lift duration overflow")?);
                    self.contact = BowContact::Lifting;
                    self.advance(at, cycle);
                }
            }
            BowAction::Stop { string, hz, over } => {
                if !hz.is_finite() || hz <= 0.0 {
                    return Err("stopped pitch must be positive and finite");
                }
                let f = self.finger(string)?;
                if hz < f.open_hz {
                    return Err("stopped pitch cannot be below the open string");
                }
                f.pitch.replace(pitch(hz), at, over, beats, cycle)?;
                f.stopped = true;
            }
            BowAction::ReleaseFinger { string, over } => {
                let f = self.finger(string)?;
                f.pitch.replace(pitch(f.open_hz), at, over, beats, cycle)?;
                f.stopped = false;
            }
            BowAction::Damp {
                string,
                amount,
                over,
            } => {
                if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
                    return Err("damping outside 0..1");
                }
                self.finger(string)?
                    .damping
                    .replace(amount, at, over, beats, cycle)?;
            }
            BowAction::Vibrato {
                string,
                cents,
                rate: hz,
                over,
            } => {
                if !cents.is_finite()
                    || !(0.0..=40.0).contains(&cents)
                    || !hz.is_finite()
                    || !(0.0..=12.0).contains(&hz)
                {
                    return Err("invalid vibrato depth/rate");
                }
                let f = self.finger(string)?;
                f.phase = f.phase_at(at, rate);
                f.phase_sample = at;
                f.rate = hz;
                f.depth.replace(cents, at, over, beats, cycle)?;
            }
            BowAction::Expression { amount, over } => {
                if !amount.is_finite() || !(0.0..=1.4).contains(&amount) {
                    return Err("expression outside 0..1.4");
                }
                self.expression.replace(amount, at, over, beats, cycle)?;
            }
        }
        Ok(())
    }
    pub fn controls(&self, sample: u64, rate: f64, string: u8) -> BowedControls {
        BowedControls {
            hz: self.fingers[string as usize].hz(sample, rate, self.cycle),
            speed: self.speed.at(sample, self.cycle),
            force: if self.contact == BowContact::Detached || self.selected != Some(string) {
                0.0
            } else {
                self.force.at(sample, self.cycle)
            },
            position: if self.selected == Some(string) {
                self.position.at(sample, self.cycle)
            } else {
                self.fingers[string as usize].bow_position
            },
        }
    }
    /// Authored dissipative contact at the same coordinate as `controls`.
    /// Offline mechanical candidates use this to trace the complete player state.
    pub fn damping(&self, sample: u64, string: u8) -> f64 {
        self.fingers[string as usize].damping.at(sample, self.cycle)
    }
    fn valid_for(&self, sample: u64, strings: &[BowedString]) -> bool {
        let (low_pos, high_pos) = self.position.remaining_bounds(sample, self.cycle);
        self.fingers
            .iter()
            .zip(strings)
            .enumerate()
            .all(|(index, (finger, string))| {
                let (low_pos, high_pos) = if self.selected == Some(index as u8) {
                    (low_pos, high_pos)
                } else {
                    (finger.bow_position, finger.bow_position)
                };
                let (low, high) = finger.pitch.remaining_bounds(sample, self.cycle);
                let depth = if finger.stopped {
                    finger.depth.remaining_bounds(sample, self.cycle).1 / 100.0
                } else {
                    0.0
                };
                let pitches = [low - depth, high + depth];
                let positions = [low_pos, high_pos];
                for (i, p) in pitches.into_iter().enumerate() {
                    if i == 1 && p == pitches[0] {
                        continue;
                    }
                    let hz = 440.0 * 2.0_f64.powf(p / 12.0);
                    for (j, pos) in positions.into_iter().enumerate() {
                        if j == 1 && pos == positions[0] {
                            continue;
                        }
                        if !string.accepts(BowedControls {
                            hz,
                            position: pos,
                            ..Default::default()
                        }) {
                            return false;
                        }
                    }
                }
                true
            })
    }
}

#[derive(Clone)]
pub struct BowedPerformance {
    config: BowedConfig,
    opens: Vec<f64>,
    strings: Vec<BowedString>,
    body: BowedBody,
    initial: BowState,
    state: BowState,
    commands: Vec<BowCommand>,
    cursor: usize,
    sample: u64,
    rate: f64,
    clock: Option<apteronotus_transport::CompiledTempo>,
    pub rejected_commands: u64,
    rate_faulted: bool,
}
impl BowedPerformance {
    pub fn new(
        config: BowedConfig,
        opens: &[f64],
        commands: Vec<BowCommand>,
        rate: f64,
    ) -> Result<Self, &'static str> {
        Self::new_clocked(config, opens, commands, rate, None)
    }
    pub fn new_clocked(
        config: BowedConfig,
        opens: &[f64],
        mut commands: Vec<BowCommand>,
        rate: f64,
        clock: Option<apteronotus_transport::CompiledTempo>,
    ) -> Result<Self, &'static str> {
        if opens.is_empty() || opens.len() > 4 || commands.len() > MAX_COMMANDS {
            return Err("bowed publication storage limit");
        }
        let mut strings = Vec::new();
        for hz in opens {
            let mut s = BowedString::new(config, rate)?;
            s.set_controls(BowedControls {
                hz: *hz,
                ..Default::default()
            })?;
            strings.push(s);
        }
        commands.sort_by_key(|c| (c.sample, c.ordinal));
        for (i, c) in commands.iter().enumerate() {
            if commands[..i].iter().any(|p| p.ordinal == c.ordinal) {
                return Err("duplicate gesture identity");
            }
            if commands[i..]
                .iter()
                .take_while(|p| p.sample == c.sample)
                .count()
                > MAX_PER_SAMPLE
                || commands[i..]
                    .iter()
                    .take_while(|p| p.sample < c.sample.saturating_add(64))
                    .count()
                    > MAX_PER_BLOCK
            {
                return Err("bowed command density budget exceeded");
            }
        }
        let initial = BowState::new(opens);
        let mut proof = initial.clone();
        for c in &commands {
            if c.beats.is_some() && clock.is_none() {
                return Err("beat ramp requires a compiled tempo clock");
            }
            proof.apply(
                *c,
                rate,
                clock
                    .as_ref()
                    .map_or(0.0, |t| t.cycle_at(c.sample as f64 / rate)),
            )?;
            if !proof.valid_for(c.sample, &strings) {
                return Err("gesture exceeds joint pitch/position domain");
            }
        }
        Ok(Self {
            config,
            opens: opens.to_vec(),
            strings,
            body: BowedBody::for_kind(rate, config.kind),
            state: initial.clone(),
            initial,
            commands,
            cursor: 0,
            sample: 0,
            rate,
            rejected_commands: 0,
            rate_faulted: false,
            clock,
        })
    }
    pub(crate) fn preroll_to(&mut self, sample: u64, live: bool) {
        if live && self.commands.is_empty() {
            self.sample = sample;
        } else {
            while self.sample < sample {
                self.next_frame();
            }
        }
    }
    pub fn state(&self) -> &BowState {
        &self.state
    }
    pub fn sample_position(&self) -> u64 {
        self.sample
    }
    pub fn diagnostics(&self) -> crate::bowed::BowedDiagnostics {
        self.strings.iter().fold(Default::default(), |mut d, s| {
            let v = s.diagnostics();
            d.rejected_controls += v.rejected_controls;
            d.emergency_resets += v.emergency_resets;
            d
        })
    }
    /// Apply one admitted operation atomically at the next unrendered sample.
    /// Host admission belongs off the audio thread; this method never allocates.
    pub fn apply_now(&mut self, action: BowAction) -> Result<(), &'static str> {
        self.apply_batch(&[Some(action)])
    }
    pub fn apply_batch(&mut self, actions: &[Option<BowAction>]) -> Result<(), &'static str> {
        if actions.len() > 4 {
            self.rejected_commands += 1;
            return Err("bowed atomic batch supports at most four actions");
        }
        self.apply_timed(actions, None)
    }
    fn apply_timed(
        &mut self,
        actions: &[Option<BowAction>],
        beats: Option<BeatRamp>,
    ) -> Result<(), &'static str> {
        let cycle = self
            .clock
            .as_ref()
            .map_or(0.0, |t| t.cycle_at(self.sample as f64 / self.rate));
        self.state.advance(self.sample, cycle);
        // Four finger states maximum; avoid cloning the Vec on the audio path.
        let backup = (
            self.state.contact,
            self.state.selected,
            self.state.speed,
            self.state.force,
            self.state.position,
            self.state.expression,
            self.state.pending_detach,
        );
        let mut fingers = [self.state.fingers[0]; 4];
        for (dst, src) in fingers.iter_mut().zip(&self.state.fingers) {
            *dst = *src;
        }
        let result = actions.iter().flatten().try_for_each(|action| {
            self.state.apply(
                BowCommand {
                    sample: self.sample,
                    ordinal: 0,
                    action: *action,
                    beats,
                },
                self.rate,
                self.state.cycle,
            )?;
            if self.state.valid_for(self.sample, &self.strings) {
                Ok(())
            } else {
                Err("gesture exceeds joint domain")
            }
        });
        if result.is_err() {
            (
                self.state.contact,
                self.state.selected,
                self.state.speed,
                self.state.force,
                self.state.position,
                self.state.expression,
                self.state.pending_detach,
            ) = backup;
            for (dst, src) in self.state.fingers.iter_mut().zip(fingers) {
                *dst = src;
            }
            self.rejected_commands += 1;
            return Err(result.err().unwrap_or("gesture exceeds joint domain"));
        }
        Ok(())
    }
    pub(crate) fn commands_due(&self) -> usize {
        self.commands[self.cursor..]
            .iter()
            .take_while(|c| c.sample == self.sample)
            .count()
    }
    pub fn next_frame(&mut self) -> [f32; 2] {
        self.frame(None)
    }
    pub fn next_live_frame(&mut self, controls: [f64; 3]) -> [f32; 2] {
        self.frame(Some(controls))
    }
    fn frame(&mut self, live: Option<[f64; 3]>) -> [f32; 2] {
        if self.rate_faulted {
            return [0.0; 2];
        }
        let cycle = self
            .clock
            .as_ref()
            .map_or(0.0, |t| t.cycle_at(self.sample as f64 / self.rate));
        self.state.advance(self.sample, cycle);
        while self.cursor < self.commands.len() && self.commands[self.cursor].sample == self.sample
        {
            let action = self.commands[self.cursor].action;
            let _ = self.apply_timed(&[Some(action)], self.commands[self.cursor].beats);
            self.cursor += 1;
        }
        let mut raw = 0.0;
        for (index, string) in self.strings.iter_mut().enumerate() {
            let mut controls = self.state.controls(self.sample, self.rate, index as u8);

            if let Some([speed, pressure, position]) = live
                && self.state.selected == Some(index as u8)
            {
                controls.speed = speed;
                controls.force *= pressure;
                controls.position = position;
            }
            if self.state.contact == BowContact::Detached
                || self.state.selected != Some(index as u8)
            {
                string.detach();
            }
            let _ = string.set_controls(controls);
            let _ = string.set_damping(
                self.state.fingers[index]
                    .damping
                    .at(self.sample, self.state.cycle),
            );
            raw += string.next_sample();
        }
        let dry = self.body.next_sample(raw)
            * self.state.expression.at(self.sample, self.state.cycle) as f32;
        self.sample += 1;
        [dry, raw]
    }
}
impl AudioUnit for BowedPerformance {
    fn reset(&mut self) {
        for s in &mut self.strings {
            s.reset();
        }
        self.body.reset();
        self.state.contact = self.initial.contact;
        self.state.selected = self.initial.selected;
        self.state.speed = self.initial.speed;
        self.state.force = self.initial.force;
        self.state.position = self.initial.position;
        self.state.expression = self.initial.expression;
        self.state.pending_detach = None;
        self.state.cycle = 0.0;
        self.state.fingers.copy_from_slice(&self.initial.fingers);
        self.sample = 0;
        self.cursor = 0;
    }
    fn set_sample_rate(&mut self, rate: f64) {
        if self.rate == rate {
            return;
        }
        let score = crate::bowed_host::BowedScore {
            config: self.config,
            live: false,
            clock: self.clock.clone(),
            opens: self.opens.clone(),
            commands: self.commands.clone(),
            coordinates: vec![],
            reference_rate: self.rate,
        };
        match score.prepare(rate) {
            Ok(new) => *self = new,
            Err(_) => {
                self.rejected_commands += 1;
                self.rate_faulted = true;
            }
        }
    }

    fn tick(&mut self, _: &[f32], output: &mut [f32]) {
        output.copy_from_slice(&self.next_frame());
    }
    fn process(&mut self, size: usize, _: &BufferRef, output: &mut BufferMut) {
        for i in 0..size {
            let y = self.next_frame();
            output.set_f32(0, i, y[0]);
            output.set_f32(1, i, y[1]);
        }
    }
    fn inputs(&self) -> usize {
        0
    }
    fn outputs(&self) -> usize {
        2
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        let mut o = SignalFrame::new(2);
        o.set(0, Signal::Unknown);
        o.set(1, Signal::Unknown);
        o
    }
    fn get_id(&self) -> u64 {
        0x4150_5442_5045_5246
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.strings.iter().map(AudioUnit::footprint).sum::<usize>()
            + self.commands.capacity() * std::mem::size_of::<BowCommand>()
    }
}

pub fn c2_config() -> BowedConfig {
    BowedConfig {
        model: BowedModel::C2,
        ..Default::default()
    }
}
