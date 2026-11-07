//! Prepared room replacement with bounded dual rendering and control-thread retirement.
//! A new room warms from continuing input, then crossfades over 100 ms. Instrument
//! state, stream topology, controls and the transport remain untouched.
use crate::spatial::{RoomSpec, RoomUnit};
use fundsp::prelude32::{AudioUnit, BufferMut, BufferRef, BufferVec, SignalFrame};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

struct Shared {
    spec: Mutex<RoomSpec>, // control-thread only
    rate: AtomicU64,
    busy: AtomicBool,
    pending: thingbuf::ThingBuf<Option<Box<RoomUnit>>>,
    retired: thingbuf::ThingBuf<Option<Box<RoomUnit>>>,
}
#[derive(Clone)]
pub struct RoomHandle(Arc<Shared>);
impl RoomHandle {
    pub fn spec(&self) -> RoomSpec {
        self.0.spec.lock().expect("room control registry").clone()
    }
    /// Preparation and reclamation belong on the control thread.
    pub fn prepare(&self, spec: RoomSpec) -> Result<RoomEdit, &'static str> {
        self.collect();
        if self.0.busy.swap(true, Ordering::AcqRel) {
            return Err("previous room edit is still warming or fading");
        }
        let current = self.spec();
        if current
            .emitters
            .iter()
            .map(|e| &e.name)
            .ne(spec.emitters.iter().map(|e| &e.name))
        {
            self.0.busy.store(false, Ordering::Release);
            return Err("room replacement must retain emitter identities and order");
        }
        match RoomUnit::new(
            spec.clone(),
            f64::from_bits(self.0.rate.load(Ordering::Acquire)),
        ) {
            Ok(unit) => Ok(RoomEdit {
                handle: self.clone(),
                spec,
                unit: Some(Box::new(unit)),
            }),
            Err(e) => {
                self.0.busy.store(false, Ordering::Release);
                Err(e)
            }
        }
    }
    pub fn collect(&self) {
        if let Some(retired) = self.0.retired.pop() {
            drop(retired);
            self.0.busy.store(false, Ordering::Release);
        }
    }
}
/// A reserved, completely prepared edit. Dropping it aborts before publication.
pub struct RoomEdit {
    handle: RoomHandle,
    spec: RoomSpec,
    unit: Option<Box<RoomUnit>>,
}
impl RoomEdit {
    pub fn commit(mut self) {
        *self.handle.0.spec.lock().expect("room control registry") = self.spec.clone();
        // One producer reservation and one packet per reservation guarantee room.
        assert!(self.handle.0.pending.push(self.unit.take()).is_ok());
    }
}
impl Drop for RoomEdit {
    fn drop(&mut self) {
        if self.unit.is_some() {
            self.handle.0.busy.store(false, Ordering::Release);
        }
    }
}
#[derive(Clone)]
pub struct ReplaceableRoom {
    handle: RoomHandle,
    current: Box<RoomUnit>,
    next: Option<Box<RoomUnit>>,
    warmup: usize,
    fade: usize,
    rate: f64,
    scratch: BufferVec,
}
impl ReplaceableRoom {
    pub fn new(spec: RoomSpec, rate: f64) -> Result<Self, &'static str> {
        let current = Box::new(RoomUnit::new(spec.clone(), rate)?);
        let handle = RoomHandle(Arc::new(Shared {
            spec: Mutex::new(spec),
            rate: AtomicU64::new(rate.to_bits()),
            busy: AtomicBool::new(false),
            pending: thingbuf::ThingBuf::new(1),
            retired: thingbuf::ThingBuf::new(1),
        }));
        Ok(Self {
            handle,
            current,
            next: None,
            warmup: 0,
            fade: 0,
            rate,
            scratch: BufferVec::new(2),
        })
    }
    pub fn handle(&self) -> RoomHandle {
        self.handle.clone()
    }
}
impl AudioUnit for ReplaceableRoom {
    fn inputs(&self) -> usize {
        self.current.inputs()
    }
    fn outputs(&self) -> usize {
        2
    }
    fn reset(&mut self) {
        self.current.reset();
        if let Some(next) = &mut self.next {
            next.reset();
        }
    }
    fn set_sample_rate(&mut self, rate: f64) {
        if !rate.is_finite() || !(8_000.0..=192_000.0).contains(&rate) {
            return;
        }
        self.current.set_sample_rate(rate);
        if let Some(next) = &mut self.next {
            next.set_sample_rate(rate);
        }
        self.rate = rate;
        self.handle.0.rate.store(rate.to_bits(), Ordering::Release);
    }
    fn process(&mut self, size: usize, input: &BufferRef, output: &mut BufferMut) {
        if self.next.is_none() {
            if let Some(Some(next)) = self.handle.0.pending.pop() {
                self.warmup = (next.response_seconds() * self.rate).ceil() as usize;
                self.fade = 0;
                self.next = Some(next);
            }
        }
        self.current.process(size, input, output);
        if let Some(next) = &mut self.next {
            next.process(size, input, &mut self.scratch.buffer_mut());
            let duration = (self.rate * 0.1).ceil() as usize;
            for i in 0..size {
                if self.warmup > 0 {
                    self.warmup -= 1;
                    continue;
                }
                self.fade = (self.fade + 1).min(duration);
                let amount = self.fade as f32 / duration as f32;
                for ear in 0..2 {
                    output.set_f32(
                        ear,
                        i,
                        output.at_f32(ear, i) * (1. - amount)
                            + self.scratch.buffer_ref().at_f32(ear, i) * amount,
                    );
                }
            }
            if self.warmup == 0 && self.fade >= duration {
                let old = std::mem::replace(&mut self.current, self.next.take().unwrap());
                // Retirement queue cannot fill: admission remains busy until
                // the control thread collects this exact old instance.
                assert!(self.handle.0.retired.push(Some(old)).is_ok());
            }
        }
    }
    fn tick(&mut self, input: &[f32], output: &mut [f32]) {
        let mut ins = fundsp::prelude32::BufferArray::<fundsp::prelude32::U32>::new();
        let mut outs = fundsp::prelude32::BufferArray::<fundsp::prelude32::U2>::new();
        for (ch, &x) in input.iter().enumerate() {
            ins.buffer_mut().set_f32(ch, 0, x);
        }
        self.process(1, &ins.buffer_ref(), &mut outs.buffer_mut());
        for (ch, x) in output.iter_mut().enumerate() {
            *x = outs.buffer_ref().at_f32(ch, 0);
        }
    }
    fn route(&mut self, input: &SignalFrame, frequency: f64) -> SignalFrame {
        self.current.route(input, frequency)
    }
    fn get_id(&self) -> u64 {
        0x4150_5350_484f_5354
    }
    fn footprint(&self) -> usize {
        std::mem::size_of::<Self>() + self.current.footprint() * 2
    }
}
