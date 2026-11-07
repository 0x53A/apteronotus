use apteronotus_lua::evaluate;
use apteronotus_synth::Op;
#[test]
fn hall_is_a_mono_to_stereo_processor_in_a_persistent_mix() {
    let program=evaluate("run(patch {inputs=2, graph=function(cv) local x=cv.inputs[1]; return x >> ((mul(1) ~ mul(1)) + concert_hall()) end})").unwrap();
    let graph=program.patches[0].graph();
    assert_eq!(graph.nodes.iter().filter(|n|matches!(n.op,Op::ConcertHall)).count(),1);
    assert_eq!(graph.outputs.len(),2);
    assert!(evaluate("concert_hall(1)").is_err());
    assert!(evaluate("run(patch {inputs=2, graph=function(cv) return cv.input >> concert_hall() end})").is_err());
}
