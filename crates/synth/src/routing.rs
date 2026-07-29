//! Program-wide bus identities and per-onset routing.
//!
//! A bus name belongs to the language's lexical environment. Once resolved,
//! the synthesis layer sees only an opaque [`BusId`]; renaming a bus cannot
//! alter its identity, and two same-spelled names in different scopes cannot
//! collide.
//!
//! There are intentionally two send mechanisms:
//!
//! - graph sends are stored on [`crate::GraphTemplate`] and can tap any
//!   internal signal;
//! - event sends live in [`EventRouting`] and copy the finished voice output.
//!
//! Keeping those distinct prevents a score-level control from pretending it
//! can address a local wire inside an instrument.

use apteronotus_pattern::Pattern;
use core::ops::Range;
use core::sync::atomic::{AtomicUsize, Ordering};

static NEXT_LAYOUT: AtomicUsize = AtomicUsize::new(1);

/// An opaque bus identity within one evaluated program.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct BusId {
    layout: usize,
    index: usize,
}

/// The flattened channel layout shared by every voice and the bus processor.
///
/// Channels are ordered as main outputs first, followed by buses in declaration
/// order. The concrete offsets are deliberately derived here rather than
/// copied into graph templates.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BusLayout {
    identity: usize,
    main_channels: usize,
    buses: Vec<usize>,
}

impl BusLayout {
    pub fn new(main_channels: usize) -> Result<BusLayout, RoutingError> {
        if main_channels == 0 {
            return Err(RoutingError::ZeroChannels);
        }
        Ok(BusLayout {
            identity: NEXT_LAYOUT.fetch_add(1, Ordering::Relaxed),
            main_channels,
            buses: Vec::new(),
        })
    }

    /// Declare a bus and return the handle the lexical environment should bind.
    pub fn add_bus(&mut self, channels: usize) -> Result<BusId, RoutingError> {
        if channels == 0 {
            return Err(RoutingError::ZeroChannels);
        }
        let id = BusId {
            layout: self.identity,
            index: self.buses.len(),
        };
        self.buses.push(channels);
        Ok(id)
    }

    pub fn main_channels(&self) -> usize {
        self.main_channels
    }

    pub fn bus_channels(&self, bus: BusId) -> Option<usize> {
        (bus.layout == self.identity)
            .then(|| self.buses.get(bus.index).copied())
            .flatten()
    }

    pub fn total_channels(&self) -> usize {
        self.main_channels + self.buses.iter().sum::<usize>()
    }

    pub fn bus_channel_counts(&self) -> &[usize] {
        &self.buses
    }

    pub fn main_range(&self) -> Range<usize> {
        0..self.main_channels
    }

    pub fn bus_range(&self, bus: BusId) -> Option<Range<usize>> {
        let channels = self.bus_channels(bus)?;
        let begin = self.main_channels + self.buses[..bus.index].iter().sum::<usize>();
        Some(begin..begin + channels)
    }

    /// Resolve an equivalent bus handle from another layout with the same
    /// logical shape. The returned handle always belongs to `target`.
    pub fn corresponding_id(&self, bus: BusId, target: &BusLayout) -> Option<BusId> {
        if bus.layout != self.identity
            || self.main_channels != target.main_channels
            || self.buses != target.buses
        {
            return None;
        }
        self.buses.get(bus.index)?;
        Some(BusId {
            layout: target.identity,
            index: bus.index,
        })
    }
}

/// A score/event-level copy of the completed voice output.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct EventSend {
    pub bus: BusId,
    pub level: f64,
}

/// A score rhythm compiled into a continuous gain envelope by the scheduler.
///
/// This remains track routing rather than a voice parameter: every routed lane
/// of the completed voice is attenuated together, including event and graph
/// sends. The scheduler owns the tempo projection because the synthesis graph
/// has only note-relative clocks.
#[derive(Clone, PartialEq, Debug)]
pub struct DuckControl {
    pub pattern: Pattern,
    pub amount: f64,
}

/// Routing values bound once per onset.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct EventRouting {
    sends: Vec<EventSend>,
    duck: Option<DuckControl>,
}

impl EventRouting {
    pub fn new() -> EventRouting {
        EventRouting::default()
    }

    pub fn send(&mut self, bus: BusId, level: f64) -> Result<&mut EventRouting, RoutingError> {
        if !level.is_finite() {
            return Err(RoutingError::NonFiniteLevel);
        }
        self.sends.push(EventSend { bus, level });
        Ok(self)
    }

    pub fn sends(&self) -> &[EventSend] {
        &self.sends
    }

    pub fn duck(
        &mut self,
        pattern: Pattern,
        amount: f64,
    ) -> Result<&mut EventRouting, RoutingError> {
        if !amount.is_finite() || !(0.0..=1.0).contains(&amount) {
            return Err(RoutingError::InvalidDuckAmount);
        }
        if self.duck.is_some() {
            return Err(RoutingError::DuplicateDuck);
        }
        self.duck = Some(DuckControl { pattern, amount });
        Ok(self)
    }

    pub fn duck_control(&self) -> Option<&DuckControl> {
        self.duck.as_ref()
    }

    pub fn merge(&mut self, other: &EventRouting) -> Result<&mut EventRouting, RoutingError> {
        for send in other.sends() {
            self.send(send.bus, send.level)?;
        }
        if let Some(duck) = other.duck_control() {
            self.duck(duck.pattern.clone(), duck.amount)?;
        }
        Ok(self)
    }

    /// Rebind arena-scoped bus handles onto an equivalent retained layout.
    pub fn rebound(
        &self,
        from: &BusLayout,
        target: &BusLayout,
    ) -> Result<EventRouting, RoutingError> {
        let mut routing = EventRouting::new();
        for send in &self.sends {
            let bus = from
                .corresponding_id(send.bus, target)
                .ok_or(RoutingError::UnknownBus(send.bus))?;
            routing.send(bus, send.level)?;
        }
        if let Some(duck) = &self.duck {
            routing.duck(duck.pattern.clone(), duck.amount)?;
        }
        Ok(routing)
    }
}

#[derive(Clone, PartialEq, Debug)]
pub enum RoutingError {
    ZeroChannels,
    MainChannelMismatch {
        template: usize,
        layout: usize,
    },
    UnknownBus(BusId),
    BusChannelMismatch {
        bus: BusId,
        expected: usize,
        found: usize,
    },
    NonFiniteLevel,
    InvalidDuckAmount,
    DuplicateDuck,
}

impl core::fmt::Display for RoutingError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RoutingError::ZeroChannels => write!(f, "a main output or bus must have channels"),
            RoutingError::MainChannelMismatch { template, layout } => write!(
                f,
                "voice has {template} main channels but the program layout has {layout}"
            ),
            RoutingError::UnknownBus(bus) => write!(f, "unknown bus {bus:?}"),
            RoutingError::BusChannelMismatch {
                bus,
                expected,
                found,
            } => write!(
                f,
                "bus {bus:?} has {expected} channels but the send has {found}"
            ),
            RoutingError::NonFiniteLevel => write!(f, "send level is not finite"),
            RoutingError::InvalidDuckAmount => {
                write!(f, "duck amount must be finite and within 0..=1")
            }
            RoutingError::DuplicateDuck => {
                write!(f, "a track may have only one duck control")
            }
        }
    }
}

impl core::error::Error for RoutingError {}
