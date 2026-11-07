use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
thread_local! {static MEASURING:Cell<bool>=const {Cell::new(false)};static ALLOCATIONS:Cell<usize>=const {Cell::new(0)};}
struct Counted;
unsafe impl GlobalAlloc for Counted {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, p: *mut u8, layout: Layout) {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.dealloc(p, layout) }
    }
    unsafe fn realloc(&self, p: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(p, layout, size) }
    }
}
#[global_allocator]
static ALLOCATOR: Counted = Counted;

#[test]
fn room_render_and_reset_neither_allocate_nor_free() {
    use apteronotus_synth::spatial::{Emitter, Listener, Material, RoomSpec, RoomUnit};
    use fundsp::prelude32::{AudioUnit, BufferVec};
    let spec = RoomSpec {
        size: [8., 7., 3.1],
        walls: Material::Wood,
        floor: Material::Wood,
        ceiling: Material::Plaster,
        decay: 0.2,
        listener: Listener {
            position: [4., 1.3, 1.25],
            facing: [0., 1., 0.],
        },
        emitters: vec![Emitter {
            name: "Lute".into(),
            position: [2.5, 3.2, 1.15],
        }],
    };
    let mut unit = RoomUnit::new(spec, 24000.).unwrap();
    let mut input = BufferVec::new(6);
    let mut output = BufferVec::new(2);
    for i in 0..64 {
        input
            .buffer_mut()
            .set_f32(0, i, (i as f32 * 0.2).sin() * 0.1);
        for ch in 1..5 {
            input.buffer_mut().set_f32(ch, i, 1.);
        }
        input.buffer_mut().set_f32(5, i, 0.2);
    }
    ALLOCATIONS.with(|n| n.set(0));
    MEASURING.with(|m| m.set(true));
    for _ in 0..100 {
        unit.process(64, &input.buffer_ref(), &mut output.buffer_mut());
    }
    unit.reset();
    let mut out = [0.; 2];
    unit.tick(&[0.1, 1., 1., 1., 1., 0.2], &mut out);
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    assert!(out.iter().all(|v| v.is_finite()));
}

#[test]
fn prepared_replacement_warms_fades_and_retires_without_allocating() {
    use apteronotus_synth::spatial::{Emitter, Listener, Material, RoomSpec};
    use apteronotus_synth::spatial_host::ReplaceableRoom;
    use fundsp::prelude32::{AudioUnit, BufferVec};
    let spec = RoomSpec {
        size: [8., 7., 3.1],
        walls: Material::Wood,
        floor: Material::Wood,
        ceiling: Material::Plaster,
        decay: 0.05,
        listener: Listener {
            position: [4., 1.3, 1.25],
            facing: [0., 1., 0.],
        },
        emitters: vec![Emitter {
            name: "test".into(),
            position: [2., 3., 1.2],
        }],
    };
    let mut unit = ReplaceableRoom::new(spec.clone(), 24000.).unwrap();
    let handle = unit.handle();
    let mut moved = spec;
    moved.emitters[0].position = [6., 3., 1.2];
    let edit = handle.prepare(moved.clone()).unwrap();
    assert!(handle.prepare(moved.clone()).is_err());
    edit.commit();
    let mut input = BufferVec::new(6);
    let mut output = BufferVec::new(2);
    for i in 0..64 {
        input.buffer_mut().set_f32(0, i, (i as f32 * 0.13).sin());
        for ch in 1..6 {
            input.buffer_mut().set_f32(ch, i, 1.);
        }
    }
    ALLOCATIONS.with(|n| n.set(0));
    MEASURING.with(|m| m.set(true));
    for _ in 0..500 {
        unit.process(64, &input.buffer_ref(), &mut output.buffer_mut());
    }
    unit.reset();
    MEASURING.with(|m| m.set(false));
    assert_eq!(ALLOCATIONS.with(Cell::get), 0);
    // Old convolvers are reclaimed only here, off the audio thread.
    let abandoned = handle.prepare(moved.clone()).unwrap();
    drop(abandoned);
    assert!(handle.prepare(moved).is_ok());
}
