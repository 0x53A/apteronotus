//! Compact execution of an already-lowered live graph. Primitive state remains
//! in FunDSP units; scalar arithmetic uses one contiguous, preallocated arena.
use crate::Op;
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, BufferVec, F32x, Shared, SignalFrame};
use fundsp::{SIMD_LEN, SIMD_M, SIMD_S, simd_items};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Value {
    Constant(f32),
    Slot(usize),
}
impl Value {
    fn resolve(self, constants: &[Option<f32>]) -> Self {
        match self {
            Self::Slot(slot) => constants[slot].map_or(self, Self::Constant),
            _ => self,
        }
    }
    fn at(self, slots: &[[F32x; SIMD_LEN]], frame: usize) -> f32 {
        match self {
            Self::Constant(value) => value,
            Self::Slot(slot) => slots[slot][frame >> SIMD_S].as_array()[frame & SIMD_M],
        }
    }
    fn vector(self, slots: &[[F32x; SIMD_LEN]], index: usize) -> F32x {
        match self {
            Self::Constant(value) => F32x::splat(value),
            Self::Slot(slot) => slots[slot][index],
        }
    }
}

#[derive(Clone, Copy)]
enum Math {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Neg,
    Clamp { min: f32, max: f32 },
}
impl Math {
    fn new(op: &Op) -> Self {
        match op {
            Op::Add => Self::Add,
            Op::Sub => Self::Sub,
            Op::Mul => Self::Mul,
            Op::Div => Self::Div,
            Op::Pow => Self::Pow,
            Op::Neg => Self::Neg,
            Op::Clamp { min, max } => Self::Clamp {
                min: *min as f32,
                max: *max as f32,
            },
            _ => unreachable!(),
        }
    }
}
#[derive(Clone)]
enum Operation {
    Math(Math, [Value; 2]),
    Shared(Shared),
    Envelope(crate::adsr::LiveEnvelopeUnit),
    Slew(crate::analyzer::SlewUnit, Value),
    Unit(Box<dyn AudioUnit>, Vec<Value>),
}
#[derive(Clone)]
struct Instruction {
    operation: Operation,
    output: usize,
    channels: usize,
}

#[derive(Clone)]
pub(crate) struct CompiledGraph {
    instructions: Vec<Instruction>,
    slots: Vec<[F32x; SIMD_LEN]>,
    constants: Vec<Option<f32>>,
    routes: Vec<(usize, Value, f32)>,
    channels: usize,
    input: BufferVec,
    sample_rate: f64,
    tick_buffer: BufferVec,
}
impl CompiledGraph {
    pub(crate) fn new(channels: usize) -> Self {
        Self {
            instructions: Vec::new(),
            slots: Vec::new(),
            constants: Vec::new(),
            routes: Vec::new(),
            channels,
            input: BufferVec::new(0),
            sample_rate: 44_100.0,
            tick_buffer: BufferVec::new(channels),
        }
    }
    fn insert(&mut self, operation: Operation, channels: usize) -> Vec<Value> {
        let output = self.slots.len();
        self.slots.resize(output + channels, [F32x::ZERO; SIMD_LEN]);
        self.instructions.push(Instruction {
            operation,
            output,
            channels,
        });
        (output..output + channels).map(Value::Slot).collect()
    }
    pub(crate) fn envelope(&mut self, mut unit: crate::adsr::LiveEnvelopeUnit) -> Vec<Value> {
        unit.set_sample_rate(self.sample_rate);
        self.insert(Operation::Envelope(unit), 1)
    }
    pub(crate) fn slew(
        &mut self,
        mut unit: crate::analyzer::SlewUnit,
        source: Value,
    ) -> Vec<Value> {
        unit.set_sample_rate(self.sample_rate);
        self.input.resize(self.input.channels().max(1));
        self.insert(Operation::Slew(unit, source), 1)
    }
    pub(crate) fn shared(&mut self, value: Shared) -> Value {
        self.insert(Operation::Shared(value), 1)[0]
    }
    pub(crate) fn math(&mut self, op: &Op, inputs: Vec<Value>) -> Vec<Value> {
        self.insert(
            Operation::Math(
                Math::new(op),
                [
                    inputs[0],
                    inputs.get(1).copied().unwrap_or(Value::Constant(0.0)),
                ],
            ),
            1,
        )
    }
    pub(crate) fn primitive(
        &mut self,
        mut unit: Box<dyn AudioUnit>,
        inputs: Vec<Value>,
    ) -> Vec<Value> {
        unit.set_sample_rate(self.sample_rate);
        self.input.resize(self.input.channels().max(unit.inputs()));
        let channels = unit.outputs();
        self.insert(Operation::Unit(unit, inputs), channels)
    }
    pub(crate) fn route(&mut self, lane: usize, value: Value, gain: f32) {
        self.routes.push((lane, value, gain));
    }
    /// Reuse scratch lanes after their final consumer. Routing values remain
    /// live through the final mix; primitive state stays in its owned unit.
    pub(crate) fn finish(&mut self) {
        let end = self.instructions.len();
        let mut last = vec![0; self.slots.len()];
        for (index, instruction) in self.instructions.iter().enumerate() {
            last[instruction.output..instruction.output + instruction.channels].fill(index);
        }
        for (index, instruction) in self.instructions.iter().enumerate() {
            let sources = match &instruction.operation {
                Operation::Math(_, sources) => sources.as_slice(),
                Operation::Unit(_, sources) => sources.as_slice(),
                Operation::Slew(_, source) => std::slice::from_ref(source),
                Operation::Shared(_) | Operation::Envelope(_) => &[],
            };
            for source in sources {
                if let Value::Slot(slot) = source {
                    last[*slot] = index;
                }
            }
        }
        for (_, source, _) in &self.routes {
            if let Value::Slot(slot) = source {
                last[*slot] = end;
            }
        }
        let mut expire = vec![Vec::new(); end + 1];
        for (slot, index) in last.into_iter().enumerate() {
            expire[index].push(slot);
        }
        let mut mapping = vec![0; self.slots.len()];
        let mut occupied = Vec::<bool>::new();
        for (index, instruction) in self.instructions.iter_mut().enumerate() {
            match &mut instruction.operation {
                Operation::Math(_, sources) => {
                    for source in sources {
                        if let Value::Slot(slot) = source {
                            *slot = mapping[*slot];
                        }
                    }
                }
                Operation::Unit(_, sources) => {
                    for source in sources {
                        if let Value::Slot(slot) = source {
                            *slot = mapping[*slot];
                        }
                    }
                }
                Operation::Slew(_, source) => {
                    if let Value::Slot(slot) = source {
                        *slot = mapping[*slot];
                    }
                }
                Operation::Shared(_) | Operation::Envelope(_) => {}
            }
            let width = instruction.channels;
            let physical = occupied
                .windows(width)
                .position(|range| range.iter().all(|used| !used))
                .unwrap_or(occupied.len());
            occupied.resize(occupied.len().max(physical + width), false);
            for channel in 0..width {
                occupied[physical + channel] = true;
                mapping[instruction.output + channel] = physical + channel;
            }
            instruction.output = physical;
            for slot in &expire[index] {
                occupied[mapping[*slot]] = false;
            }
        }
        for (_, source, _) in &mut self.routes {
            if let Value::Slot(slot) = source {
                *slot = mapping[*slot];
            }
        }
        self.slots.resize(occupied.len(), [F32x::ZERO; SIMD_LEN]);
        self.constants.resize(occupied.len(), None);
    }
    fn render(&mut self, size: usize, output: &mut BufferMut) {
        if size == 0 {
            return;
        }
        let vectors = simd_items(size);
        for instruction in &mut self.instructions {
            match &mut instruction.operation {
                Operation::Shared(value) => {
                    self.constants[instruction.output] = Some(value.value())
                }
                Operation::Envelope(unit) => {
                    let constant = unit.constant_block(size);
                    self.constants[instruction.output] = constant;
                    if constant.is_none() {
                        unit.process(
                            size,
                            &BufferRef::empty(),
                            &mut BufferMut::new(self.slots[instruction.output].as_mut_slice()),
                        );
                    }
                }
                Operation::Slew(unit, source) => {
                    let source = source.resolve(&self.constants);
                    let constant = match source {
                        Value::Constant(input) => unit.constant_output(input),
                        _ => None,
                    };
                    self.constants[instruction.output] = constant;
                    if constant.is_none() {
                        let mut input = self.input.buffer_mut();
                        match source {
                            Value::Constant(value) => {
                                input.channel_mut(0)[..vectors].fill(F32x::splat(value))
                            }
                            Value::Slot(slot) => input.channel_mut(0)[..vectors]
                                .copy_from_slice(&self.slots[slot][..vectors]),
                        }
                        unit.process(
                            size,
                            &self.input.buffer_ref().subset(0, 1),
                            &mut BufferMut::new(self.slots[instruction.output].as_mut_slice()),
                        );
                    }
                }
                Operation::Math(op, sources) => {
                    let inputs = &self.slots;
                    let a = sources[0].resolve(&self.constants);
                    let b = sources
                        .get(1)
                        .copied()
                        .unwrap_or(Value::Constant(0.0))
                        .resolve(&self.constants);
                    if let (Value::Constant(a), Value::Constant(b)) = (a, b) {
                        let value = match op {
                            Math::Add => a + b,
                            Math::Sub => a - b,
                            Math::Mul => a * b,
                            Math::Div => a / b,
                            Math::Pow => a.powf(b),
                            Math::Neg => -a,
                            Math::Clamp { min, max } => a.clamp(*min, *max),
                        };
                        self.constants[instruction.output] = Some(value);
                        continue;
                    }
                    self.constants[instruction.output] = None;
                    let mut result = [F32x::ZERO; SIMD_LEN];
                    match op {
                        Math::Add => {
                            for (i, out) in result[..vectors].iter_mut().enumerate() {
                                *out = a.vector(inputs, i) + b.vector(inputs, i)
                            }
                        }
                        Math::Sub => {
                            for (i, out) in result[..vectors].iter_mut().enumerate() {
                                *out = a.vector(inputs, i) - b.vector(inputs, i)
                            }
                        }
                        Math::Mul => {
                            for (i, out) in result[..vectors].iter_mut().enumerate() {
                                *out = a.vector(inputs, i) * b.vector(inputs, i)
                            }
                        }
                        Math::Div => {
                            for (i, out) in result[..vectors].iter_mut().enumerate() {
                                *out = a.vector(inputs, i) / b.vector(inputs, i)
                            }
                        }
                        Math::Pow => {
                            for i in 0..size {
                                result[i >> SIMD_S].as_mut_array()[i & SIMD_M] =
                                    a.at(inputs, i).powf(b.at(inputs, i));
                            }
                        }
                        Math::Neg => {
                            for (i, out) in result[..vectors].iter_mut().enumerate() {
                                *out = -a.vector(inputs, i)
                            }
                        }
                        Math::Clamp { min, max } => {
                            for i in 0..size {
                                result[i >> SIMD_S].as_mut_array()[i & SIMD_M] =
                                    a.at(inputs, i).clamp(*min, *max);
                            }
                        }
                    }
                    self.slots[instruction.output][..vectors].copy_from_slice(&result[..vectors]);
                }
                Operation::Unit(unit, sources) => {
                    self.constants[instruction.output..instruction.output + instruction.channels]
                        .fill(None);
                    let direct = match sources.as_slice() {
                        [source] => match source.resolve(&self.constants) {
                            Value::Slot(slot) => Some(slot),
                            _ => None,
                        },
                        _ => None,
                    };
                    if direct.is_none() {
                        for (channel, source) in sources.iter().enumerate() {
                            let mut buffer = self.input.buffer_mut();
                            let target = &mut buffer.channel_mut(channel)[..vectors];
                            match source.resolve(&self.constants) {
                                Value::Constant(value) => target.fill(F32x::splat(value)),
                                Value::Slot(slot) => {
                                    target.copy_from_slice(&self.slots[slot][..vectors])
                                }
                            }
                        }
                    }
                    // Output lanes are contiguous and disjoint from every
                    // live input, established by the scratch-lifetime pass.
                    let (before, tail) = self.slots.split_at_mut(instruction.output);
                    let (result, after) = tail.split_at_mut(instruction.channels);
                    let scratch = self.input.buffer_ref();
                    let input = if let Some(slot) = direct {
                        let lane = if slot < instruction.output {
                            &before[slot]
                        } else {
                            &after[slot - instruction.output - instruction.channels]
                        };
                        BufferRef::new(lane.as_slice())
                    } else {
                        scratch.subset(0, unit.inputs())
                    };
                    unit.process(size, &input, &mut BufferMut::new(result.as_flattened_mut()));
                }
            }
        }
        for channel in 0..self.channels {
            output.channel_mut(channel)[..vectors].fill(F32x::ZERO);
        }
        for &(lane, source, gain) in &self.routes {
            let source = source.resolve(&self.constants);
            let target = &mut output.channel_mut(lane)[..vectors];
            for (i, out) in target.iter_mut().enumerate() {
                *out += source.vector(&self.slots, i) * gain;
            }
        }
    }
}
impl AudioUnit for CompiledGraph {
    fn reset(&mut self) {
        for instruction in &mut self.instructions {
            if let Operation::Unit(unit, _) = &mut instruction.operation {
                unit.reset()
            } else if let Operation::Envelope(unit) = &mut instruction.operation {
                unit.reset();
            } else if let Operation::Slew(unit, _) = &mut instruction.operation {
                unit.reset();
            }
        }
    }
    fn set_sample_rate(&mut self, rate: f64) {
        self.sample_rate = rate;
        for instruction in &mut self.instructions {
            if let Operation::Unit(unit, _) = &mut instruction.operation {
                unit.set_sample_rate(rate)
            } else if let Operation::Envelope(unit) = &mut instruction.operation {
                unit.set_sample_rate(rate);
            } else if let Operation::Slew(unit, _) = &mut instruction.operation {
                unit.set_sample_rate(rate);
            }
        }
    }
    fn tick(&mut self, _: &[f32], output: &mut [f32]) {
        // Tick shares the exact block executor; the scratch result never allocates.
        let mut buffer = std::mem::take(&mut self.tick_buffer);
        self.render(1, &mut buffer.buffer_mut());
        for (channel, out) in output.iter_mut().enumerate() {
            *out = buffer.buffer_ref().at_f32(channel, 0)
        }
        self.tick_buffer = buffer;
    }
    fn process(&mut self, size: usize, _: &BufferRef, output: &mut BufferMut) {
        self.render(size, output);
    }
    fn inputs(&self) -> usize {
        0
    }
    fn outputs(&self) -> usize {
        self.channels
    }
    fn route(&mut self, _: &SignalFrame, _: f64) -> SignalFrame {
        SignalFrame::new(self.channels)
    }
    fn allocate(&mut self) {
        for instruction in &mut self.instructions {
            if let Operation::Unit(unit, _) = &mut instruction.operation {
                unit.allocate()
            }
        }
    }
    fn get_id(&self) -> u64 {
        0x4150_5443_4f4d_5047
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.slots.len() * std::mem::size_of::<[F32x; SIMD_LEN]>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Adsr, LiveGate};
    use fundsp::prelude32::shared;

    #[test]
    fn reused_lanes_preserve_fanout_controls_envelopes_and_partial_blocks() {
        let control = shared(0.7);
        let gate = LiveGate::new();
        let mut graph = CompiledGraph::new(3);
        let level = graph.shared(control.clone());
        let oscillator = graph.primitive(
            Box::new(crate::harmonic::HarmonicUnit::new(&[0.5, 0.3, 0.2])),
            vec![Value::Constant(220.0)],
        )[0];
        let envelope = graph.envelope(crate::adsr::LiveEnvelopeUnit::adsr(
            Adsr::new(0.01, 0.02, 0.72, 0.05),
            gate.clone(),
        ))[0];
        let voice = graph.math(&Op::Mul, vec![oscillator, envelope])[0];
        let scaled = graph.math(&Op::Mul, vec![voice, level])[0];
        let square = graph.math(&Op::Mul, vec![voice, voice])[0];
        let biased = graph.math(&Op::Add, vec![square, Value::Constant(0.1)])[0];
        graph.route(0, scaled, 1.0);
        graph.route(1, voice, 0.5);
        graph.route(2, biased, 1.0);
        let mut reference = graph.clone();
        reference.constants.resize(reference.slots.len(), None);
        graph.finish();
        assert!(graph.slots.len() < reference.slots.len());
        for unit in [&mut graph, &mut reference] {
            unit.set_sample_rate(8000.0);
        }
        let input = BufferVec::new(0);
        let mut expected = BufferVec::new(3);
        let mut actual = BufferVec::new(3);
        for block in 0..80 {
            if block == 20 {
                control.set(0.2);
            }
            if block == 40 {
                gate.release_after(0.2).unwrap();
            }
            let size = [1, 7, 17, 64][block % 4];
            reference.process(size, &input.buffer_ref(), &mut expected.buffer_mut());
            graph.process(size, &input.buffer_ref(), &mut actual.buffer_mut());
            for channel in 0..3 {
                for frame in 0..size {
                    assert_eq!(
                        actual.buffer_ref().at_f32(channel, frame),
                        expected.buffer_ref().at_f32(channel, frame)
                    );
                }
            }
        }
    }
}
