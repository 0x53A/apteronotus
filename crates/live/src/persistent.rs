//! Persistent program-scope audio around the per-note sequencer.
//!
//! The sequencer produces one flattened main/bus stem layout. Autonomous
//! zero-input `run` patches are mixed into those lanes, then full-layout input
//! patches process the result in declaration order. The processor stays alive
//! for the program lifetime, so delay, oscillator, and filter state survive
//! scheduler windows.

use apteronotus_synth::{
    AudioInputLayout, BusLayout, ControlLayout, ControlStore, LowerError, PatchTemplate,
    instantiate_patch_routed_at, instantiate_patch_routed_with_audio_inputs_at,
};
use fundsp::net::Net;
use fundsp::prelude32::{AudioUnit, ReplayMode, Sequencer, pass, zero};

/// One instantiated persistent arena for an evaluated program.
///
/// The audio processor is moved into the device graph exactly once. The
/// [`ControlStore`] remains on the control thread; its shared atomics are the
/// only connection to the audio-thread instances.
pub struct PersistentRuntime {
    layout: BusLayout,
    controls: ControlStore,
    processor: Option<Box<dyn AudioUnit>>,
    runs: usize,
    external_channels: usize,
}

impl PersistentRuntime {
    pub fn new<'a>(
        layout: &BusLayout,
        controls: &ControlLayout,
        patches: impl IntoIterator<Item = &'a PatchTemplate>,
    ) -> Result<PersistentRuntime, PersistentError> {
        Self::new_at_transport(layout, controls, patches, 0.0)
    }

    /// Construct a persistent arena at an absolute transport time.
    ///
    /// This coordinate initializes derived transport controls without
    /// pretending stateful DSP has history it has not rendered.
    pub fn new_at_transport<'a>(
        layout: &BusLayout,
        controls: &ControlLayout,
        patches: impl IntoIterator<Item = &'a PatchTemplate>,
        transport_seconds: f64,
    ) -> Result<PersistentRuntime, PersistentError> {
        Self::new_inner(layout, controls, None, patches, transport_seconds)
    }

    /// Construct a persistent arena whose processor exposes the program's
    /// flattened logical audio inputs after the routed stem lanes.
    pub fn with_audio_inputs<'a>(
        layout: &BusLayout,
        controls: &ControlLayout,
        audio_inputs: &AudioInputLayout,
        patches: impl IntoIterator<Item = &'a PatchTemplate>,
    ) -> Result<PersistentRuntime, PersistentError> {
        Self::with_audio_inputs_at_transport(layout, controls, audio_inputs, patches, 0.0)
    }

    /// Construct an input-bearing persistent arena at an absolute transport
    /// time. See [`Self::new_at_transport`].
    pub fn with_audio_inputs_at_transport<'a>(
        layout: &BusLayout,
        controls: &ControlLayout,
        audio_inputs: &AudioInputLayout,
        patches: impl IntoIterator<Item = &'a PatchTemplate>,
        transport_seconds: f64,
    ) -> Result<PersistentRuntime, PersistentError> {
        Self::new_inner(
            layout,
            controls,
            Some(audio_inputs),
            patches,
            transport_seconds,
        )
    }

    fn new_inner<'a>(
        layout: &BusLayout,
        controls: &ControlLayout,
        audio_inputs: Option<&AudioInputLayout>,
        patches: impl IntoIterator<Item = &'a PatchTemplate>,
        transport_seconds: f64,
    ) -> Result<PersistentRuntime, PersistentError> {
        let controls = ControlStore::new(controls);
        let lanes = layout.total_channels();
        let external_channels = audio_inputs.map_or(0, AudioInputLayout::total_channels);
        let mut sources = Vec::new();
        let mut processors = Vec::new();
        let mut runs = 0;

        for (index, patch) in patches.into_iter().enumerate() {
            let inputs = patch.graph().inputs;
            if inputs != 0 && inputs != lanes {
                return Err(PersistentError::RunInputChannels {
                    index,
                    expected: lanes,
                    found: inputs,
                });
            }
            let unit = match audio_inputs {
                Some(audio_inputs) => instantiate_patch_routed_with_audio_inputs_at(
                    patch,
                    layout,
                    &controls,
                    audio_inputs,
                    transport_seconds,
                ),
                None => instantiate_patch_routed_at(patch, layout, &controls, transport_seconds),
            }
            .map_err(|source| PersistentError::Lower { index, source })?;
            if inputs == 0 {
                sources.push(unit);
            } else {
                processors.push(unit);
            }
            runs += 1;
        }

        let processor = build_processor(lanes, external_channels, sources, processors);
        Ok(PersistentRuntime {
            layout: layout.clone(),
            controls,
            processor: Some(Box::new(processor)),
            runs,
            external_channels,
        })
    }

    pub fn layout(&self) -> &BusLayout {
        &self.layout
    }

    pub fn controls(&self) -> &ControlStore {
        &self.controls
    }

    pub fn runs(&self) -> usize {
        self.runs
    }

    pub fn sequencer(&self) -> Sequencer {
        Sequencer::new(0, self.layout.total_channels(), ReplayMode::None)
    }

    pub fn external_channels(&self) -> usize {
        self.external_channels
    }

    /// Move the realtime unit into an offline/fallback graph.
    ///
    /// Logical audio inputs are supplied with their declared silence fallback.
    /// The native host uses [`take_processor_with_audio_inputs`](Self::take_processor_with_audio_inputs)
    /// instead.
    ///
    /// A runtime represents one persistent program generation, so taking its
    /// processor twice is a host bug rather than a recoverable edit error.
    pub fn take_processor(&mut self) -> Box<dyn AudioUnit> {
        let processor = self
            .processor
            .take()
            .expect("a persistent processor can only be installed once");
        if self.external_channels == 0 {
            return processor;
        }

        let lanes = self.layout.total_channels();
        let mut net = Net::new(lanes, lanes);
        let processor = net.push(processor);
        for lane in 0..lanes {
            let input = net.push(Box::new(pass()));
            net.connect_input(lane, input, 0);
            net.connect(input, 0, processor, lane);
        }
        for channel in 0..self.external_channels {
            let fallback = net.push(Box::new(zero()));
            net.connect(fallback, 0, processor, lanes + channel);
        }
        for lane in 0..lanes {
            net.connect_output(processor, lane, lane);
        }
        Box::new(net)
    }

    /// Move the realtime unit into a host graph that supplies live audio lanes.
    pub fn take_processor_with_audio_inputs(&mut self) -> Box<dyn AudioUnit> {
        self.processor
            .take()
            .expect("a persistent processor can only be installed once")
    }
}

fn build_processor(
    lanes: usize,
    external_channels: usize,
    sources: Vec<Box<dyn AudioUnit>>,
    processors: Vec<Box<dyn AudioUnit>>,
) -> Net {
    let mut net = Net::new(lanes + external_channels, lanes);

    // Start each lane as an exact pass-through from the voice sequencer.
    let mut current = Vec::with_capacity(lanes);
    for lane in 0..lanes {
        let node = net.push(Box::new(pass()));
        net.connect_input(lane, node, 0);
        current.push((node, 0));
    }

    // Autonomous patches run in parallel with voices and sum into the same
    // routed lanes. Their graph sends have already been flattened by synth.
    for source in sources {
        let source = net.push(source);
        for channel in 0..external_channels {
            net.connect_input(lanes + channel, source, channel);
        }
        for (lane, current) in current.iter_mut().enumerate() {
            let sum = net.push(Box::new(pass() + pass()));
            let (prior, port) = *current;
            net.connect(prior, port, sum, 0);
            net.connect(source, lane, sum, 1);
            *current = (sum, 0);
        }
    }

    // An input-bearing run is explicitly a whole-stem processor. Exact arity
    // avoids modulo wiring, which would silently fold one bus into another.
    for processor in processors {
        let processor = net.push(processor);
        for (lane, &(source, port)) in current.iter().enumerate() {
            net.connect(source, port, processor, lane);
        }
        for channel in 0..external_channels {
            net.connect_input(lanes + channel, processor, lanes + channel);
        }
        current = (0..lanes).map(|lane| (processor, lane)).collect();
    }

    for (lane, (source, port)) in current.into_iter().enumerate() {
        net.connect_output(source, port, lane);
    }
    net
}

#[derive(Debug)]
pub enum PersistentError {
    RunInputChannels {
        index: usize,
        expected: usize,
        found: usize,
    },
    Lower {
        index: usize,
        source: LowerError,
    },
}

impl core::fmt::Display for PersistentError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PersistentError::RunInputChannels {
                index,
                expected,
                found,
            } => write!(
                f,
                "run {index} has {found} inputs; a persistent run must have zero inputs \
                 or consume all {expected} flattened main/bus lanes"
            ),
            PersistentError::Lower { index, source } => {
                write!(f, "cannot instantiate persistent run {index}: {source}")
            }
        }
    }
}

impl core::error::Error for PersistentError {}
