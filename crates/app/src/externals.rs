//! Host-injected external sources. A host registers them before Run; evaluation
//! receives an immutable snapshot, never a path resolver or filesystem callback.
use apteronotus_lua::{Evaluator,ExternalRegistry,Program};

/// Combined declared payload the app admits for registered external sources.
pub(crate) const MAX_PAYLOAD:usize=256*1024*1024;

pub(crate) fn evaluate(evaluator:Evaluator,source:&str,externals:&ExternalRegistry)->Result<Program,String> {
    if externals.payload_bytes()>MAX_PAYLOAD {
        return Err("app external sources exceed the 256 MiB payload allowance".into());
    }
    let mut limits=evaluator.limits();
    // Keep the ordinary graph allowance, adding only the explicitly registered
    // payload. This counts immutable data, not prepared DSP heap.
    limits.graph_publication.data_entries=limits.graph_publication.data_entries
        .checked_add(externals.payload_bytes().div_ceil(8)).ok_or("external payload allowance overflow")?;
    Evaluator::new(limits).evaluate_with_externals(source,externals).map_err(|e|e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use apteronotus_synth::{External,ExternalSource,PreparedExternal};
    #[derive(Debug)]
    struct Stub;
    impl ExternalSource for Stub {
        fn kind(&self)->&str {"stub"}
        fn outputs(&self)->usize {2}
        fn data_entries(&self)->usize {16}
        fn prepare(&self,_:f64,_:f64)->Result<PreparedExternal,String> {Err("unused".into())}
    }
    const STARTER:&str="run(external('performance'))";
    fn registry()->ExternalRegistry {
        let mut registry=ExternalRegistry::new(1,4096);registry.register("performance",External::new(Stub)).unwrap();registry
    }
    #[test]
    fn run_request_owns_registry_snapshot_for_background_evaluation() {
        let externals=registry();
        let request=crate::player::Command::Run{request:7,source:STARTER.into(),externals:externals.clone()};
        drop(externals);
        let program=std::thread::spawn(move || {
            let crate::player::Command::Run{request,source,externals}=request else {panic!()};
            assert_eq!(request,7);evaluate(Evaluator::default(),&source,&externals).unwrap()
        }).join().unwrap();
        assert_eq!(program.runs.len(),1);
        assert!(program.patches[0].graph().nodes.iter().any(|n|matches!(n.op,apteronotus_synth::Op::External(_))));
        assert!(evaluate(Evaluator::default(),STARTER,&ExternalRegistry::default()).unwrap_err().contains("not registered"));
    }
    #[test]
    fn ordinary_sources_keep_existing_limits() {
        assert!(evaluate(Evaluator::default(),"run(patch{graph=function()return dc(0)end})",&ExternalRegistry::default()).is_ok());
        let mut limits=apteronotus_lua::Limits::default();limits.graph_nodes=0;
        assert!(evaluate(Evaluator::new(limits),STARTER,&registry()).is_err());
    }
}
