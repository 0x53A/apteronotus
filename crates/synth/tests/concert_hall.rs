use apteronotus_synth::{
    BusLayout, ControlLayout, ControlStore, GraphBuilder, PatchTemplate,
    concert_hall::{HallUnit, preset},
};
use fundsp::prelude32::{AudioUnit, BufferVec};

#[test]
fn graph_hall_matches_prepared_kernel_and_preserves_history_at_same_rate() {
    let mut graph = GraphBuilder::with_inputs(1);
    let x = graph.input(0);
    let outputs = graph.concert_hall(x);
    let graph = graph.out(&outputs).unwrap();
    assert_eq!(graph.lifetime().gate_tail, preset::TAIL_SECONDS);
    assert!(graph.cost().delay_buffer_seconds > 0.);
    let patch = PatchTemplate::new(graph).unwrap();
    let store = ControlStore::new(&ControlLayout::new());
    let buses = BusLayout::new(2).unwrap();
    assert!(
        apteronotus_synth::prepare_patch_routed(&patch, &buses, &store, None, 0., 44100.).is_err()
    );
    let mut actual =
        apteronotus_synth::prepare_patch_routed(&patch, &buses, &store, None, 0., 48000.).unwrap();
    let mut reference = preset::prepare(48000.).unwrap();
    let mut peak = 0_f32;
    for i in 0..384000 {
        if i == 12345 {
            actual.set_sample_rate(48000.);
        }
        let x = if i == 0 { 1. } else { 0. };
        let expected = reference.tick(x);
        let mut out = [0.; 2];
        actual.tick(&[x as f32], &mut out);
        assert_eq!(out, expected.map(|v| v as f32));
        peak = peak.max(out[0].abs()).max(out[1].abs());
        if i == 0 {
            assert_eq!(out, [0.; 2]);
        } // Reflections exclude the direct path.
    }
    assert!(peak > 0.01);
    actual.reset();
    let mut out = [1.; 2];
    actual.tick(&[0.], &mut out);
    assert_eq!(out, [0.; 2]);
}

#[test]
fn block_partition_clone_reset_and_wrong_clock_are_explicit() {
    let mut unit = HallUnit::new(48000.).unwrap();
    let mut reference = preset::prepare(48000.).unwrap();
    let mut input = BufferVec::new(1);
    let mut output = BufferVec::new(2);
    let mut frame = 0;
    for size in [1, 7, 64, 31].into_iter().cycle().take(900) {
        for i in 0..size {
            input
                .buffer_mut()
                .set_f32(0, i, if (frame + i) % 811 == 0 { 0.5 } else { 0. });
        }
        unit.process(size, &input.buffer_ref(), &mut output.buffer_mut());
        for i in 0..size {
            let expected = reference.tick(input.buffer_ref().at_f32(0, i) as f64);
            for ch in 0..2 {
                assert_eq!(output.buffer_ref().at_f32(ch, i), expected[ch] as f32);
            }
        }
        frame += size;
    }
    let mut copy = unit.clone();
    for _ in 0..512 {
        let mut a = [0.; 2];
        let mut b = a;
        unit.tick(&[0.], &mut a);
        copy.tick(&[0.], &mut b);
        assert_eq!(a, b);
    }
    unit.set_sample_rate(44100.);
    let mut out = [1.; 2];
    unit.tick(&[1.], &mut out);
    assert_eq!(out, [0.; 2]);
    unit.set_sample_rate(48000.);
    unit.tick(&[0.], &mut out);
    assert_eq!(out, [0.; 2]);
    for rate in [44100., 96000., f64::NAN] {
        assert!(HallUnit::new(rate).is_err());
    }
}

struct CountAlloc;
thread_local! {
    static COUNT: std::cell::Cell<Option<usize>> = const { std::cell::Cell::new(None) };
}
fn operation() {
    let _ = COUNT.try_with(|c| {
        if let Some(n) = c.get() {
            c.set(Some(n + 1));
        }
    });
}
unsafe impl std::alloc::GlobalAlloc for CountAlloc {
    unsafe fn alloc(&self, l: std::alloc::Layout) -> *mut u8 {
        operation();
        unsafe { std::alloc::System.alloc(l) }
    }
    unsafe fn alloc_zeroed(&self, l: std::alloc::Layout) -> *mut u8 {
        operation();
        unsafe { std::alloc::System.alloc_zeroed(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: std::alloc::Layout) {
        operation();
        unsafe { std::alloc::System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: std::alloc::Layout, n: usize) -> *mut u8 {
        operation();
        unsafe { std::alloc::System.realloc(p, l, n) }
    }
}
#[global_allocator]
static ALLOCATOR: CountAlloc = CountAlloc;
#[test]
fn adapter_processing_reset_and_rate_notifications_have_no_heap_operations() {
    let mut hall = HallUnit::new(48000.).unwrap();
    let mut input = BufferVec::new(1);
    input.clear();
    let mut output = BufferVec::new(2);
    COUNT.with(|c| c.set(Some(0)));
    for _ in 0..100 {
        hall.process(64, &input.buffer_ref(), &mut output.buffer_mut());
    }
    hall.reset();
    hall.set_sample_rate(44100.);
    hall.set_sample_rate(48000.);
    let count = COUNT.with(|c| c.replace(None).unwrap());
    assert_eq!(count, 0);
}
