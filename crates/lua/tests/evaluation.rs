use apteronotus_lua::{EvalError, Evaluator, Limits, evaluate};
use apteronotus_pattern::{ControlValue, CurveClock, Frac, Span, Value};
use apteronotus_synth::{
    GraphLimits, Note, Op, instantiate,
    lower::{render, rms, zero_crossing_hz},
};

const SR: f64 = 48_000.0;

#[test]
fn an_edit_stages_a_polyphonic_voice_and_pattern_as_plain_data() {
    let program = evaluate(
        r#"
        tone = voice {
          params = {
            gain = { 0, 1, 0.4 },
            tone = { min = 0.5, max = 4, default = 2 },
          },
          graph = function(n)
            local hz = n.hz * n.tone
            local oscillator = sine(hz)
            local amplitude = n.velocity * dc(n.gain)
            return pan(oscillator * amplitude, n.pan)
          end,
        }

        play(tone, pattern("a4 [~ c5]"))
        "#,
    )
    .unwrap();

    assert_eq!(program.voices.len(), 1);
    assert_eq!(program.tracks.len(), 1);
    let voice = &program.voices[0];
    assert_eq!(voice.channels(), 2);
    // Parameter indices are name-sorted, independent of Lua hash iteration.
    assert_eq!(voice.params[0].name, "gain");
    assert_eq!(voice.params[1].name, "tone");
    assert!(voice.nodes.iter().any(|node| node.op == Op::Sine));
    assert!(voice.nodes.iter().any(|node| node.op == Op::Pan));

    let events = program.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].value, Value::text("a4"));
    assert_eq!(events[1].part.begin, Frac::new(3, 4));

    let mut unit = instantiate(voice, &Note::new(220.0).velocity(0.8)).unwrap();
    let audio = render(unit.as_mut(), SR, 0.2);
    assert!((zero_crossing_hz(&audio[0], SR) - 440.0).abs() < 3.0);
    assert!(rms(&audio[0]) > 0.05);
}

#[test]
fn closures_varargs_loops_and_table_library_can_generate_topology() {
    let program = evaluate(
        r#"
        local function bank(n, ...)
          local ratios = table.pack(...)
          table.insert(ratios, 1.5)
          table.sort(ratios)
          assert(table.concat(ratios, ",") == "1,1.5,2,3")
          assert(table.remove(ratios) == 3)

          local modes = {}
          for i, ratio in ipairs(ratios) do
            modes[i] = mul(sine(mul(n.hz, ratio)), 1 / i)
          end
          return mix(modes)
        end

        local instrument = voice {
          graph = function(n)
            local function closed_over_note(...)
              return bank(n, ...)
            end
            return closed_over_note(3, 1, 2)
          end,
        }
        play(instrument, "c4")
        "#,
    )
    .unwrap();

    let voice = &program.voices[0];
    assert_eq!(
        voice
            .nodes
            .iter()
            .filter(|node| node.op == Op::Sine)
            .count(),
        3
    );
    assert_eq!(program.tracks.len(), 1);
}

#[test]
fn graph_failure_can_be_caught_without_leaking_builder_state() {
    let program = evaluate(
        r#"
        local ok = pcall(function()
          voice { graph = function(n) error("bad graph") end }
        end)
        assert(not ok)

        local good = voice {
          graph = function(n) return sine(n.hz) end,
        }
        play(good, "a4")
        "#,
    )
    .unwrap();

    assert_eq!(program.voices.len(), 1);
    assert_eq!(program.tracks.len(), 1);
}

#[test]
fn sandbox_omits_host_access_dynamic_loading_and_randomness() {
    let program = evaluate(
        r#"
        assert(io == nil and os == nil and package == nil and debug == nil)
        assert(load == nil and loadfile == nil and dofile == nil)
        assert(not pcall(print, "no output"))
        assert(not pcall(collectgarbage, "count"))
        assert(not pcall(math.random))
        assert(not pcall(math.randomseed, 1))
        assert(string.upper("electric") == "ELECTRIC")
        "#,
    )
    .unwrap();
    assert_eq!(program.voices.len(), 0);
}

#[test]
fn duration_units_are_checked_at_their_binding_boundaries() {
    evaluate(
        r#"
        local timed = voice {
          graph = function(n)
            return sine(n.hz)
                >> delay(n.duration + ms(5), { min = 0, max = secs(8) })
          end,
        }
        play(timed, pattern("c4") >> shift(bars(0.25)))
        "#,
    )
    .unwrap();

    let graph_error = evaluate(
        r#"
        local timed = voice {
          graph = function(n) return sine(n.hz) >> delay(bars(0.25)) end,
        }
        play(timed, "c4")
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        graph_error.contains("expects seconds") && graph_error.contains("bars"),
        "{graph_error}"
    );

    let pattern_error = evaluate(
        r#"
        local timed = voice {
          graph = function(n) return sine(n.hz) end,
        }
        play(timed, pattern("c4") >> shift(secs(0.25)))
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        pattern_error.contains("bars/cycle time, not seconds"),
        "{pattern_error}"
    );

    let ratio_error = evaluate(
        r#"
        local timed = voice {
          graph = function(n) return sine(n.hz) end,
        }
        play(timed, pattern("c4") >> fast(bars(2)))
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(
        ratio_error.contains("fast factor must be a finite number"),
        "{ratio_error}"
    );
}

#[test]
fn runaway_code_is_interrupted_by_fuel() {
    let limits = Limits {
        fuel: 2_000,
        ..Limits::default()
    };
    let error = Evaluator::new(limits)
        .evaluate("while true do end")
        .unwrap_err();
    assert!(matches!(error, EvalError::FuelLimit { limit: 2_000 }));
}

#[test]
fn memory_is_bounded() {
    let limits = Limits {
        memory_bytes: 1,
        ..Limits::default()
    };
    let error = Evaluator::new(limits).evaluate("").unwrap_err();
    assert!(matches!(error, EvalError::MemoryLimit { limit: 1, .. }));
}

#[test]
fn graph_growth_is_bounded_separately_from_lua_memory() {
    let limits = Limits {
        graph_nodes: 1,
        ..Limits::default()
    };
    let error = Evaluator::new(limits)
        .evaluate(
            r#"
            voice {
              graph = function(n)
                return mul(sine(n.hz), n.velocity)
              end,
            }
            "#,
        )
        .unwrap_err();
    assert!(error.to_string().contains("graph node limit"));
}

#[test]
fn pattern_growth_is_bounded_separately_from_lua_memory() {
    let limits = Limits {
        pattern_nodes: 5,
        ..Limits::default()
    };
    let error = Evaluator::new(limits)
        .evaluate(r#"pattern("a b c d e f")"#)
        .unwrap_err();
    assert!(error.to_string().contains("pattern node limit"));
}

#[test]
fn symbolic_values_cannot_cross_voice_boundaries() {
    let program = evaluate(
        r#"
        local leaked
        local first = voice {
          graph = function(n)
            leaked = n.hz
            return sine(n.hz)
          end,
        }

        local ok = pcall(function()
          voice {
            graph = function(n) return sine(leaked) end,
          }
        end)
        assert(not ok)
        play(first, "a4")
        "#,
    )
    .unwrap();
    assert_eq!(program.voices.len(), 1);
}

#[test]
fn voice_handles_are_typed_not_forgeable_indices() {
    let program = evaluate(
        r#"
        local instrument = voice {
          graph = function(n) return sine(n.hz) end,
        }
        assert(not pcall(play, 1, "a4"))
        play(instrument, "a4")
        "#,
    )
    .unwrap();
    assert_eq!(program.tracks.len(), 1);
}

#[test]
fn symbolic_arithmetic_uses_lua_operator_metamethods() {
    let program = evaluate(
        r#"
        voice {
          graph = function(n)
            return sine(2 * n.hz + -n.pan * 0)
          end,
        }
        "#,
    )
    .unwrap();
    let voice = &program.voices[0];
    assert!(voice.nodes.iter().any(|node| node.op == Op::Mul));
    assert!(voice.nodes.iter().any(|node| node.op == Op::Add));
    assert!(voice.nodes.iter().any(|node| node.op == Op::Neg));
}

#[test]
fn implicit_note_names_match_the_synth_contract_without_gate_aliases() {
    evaluate(
        r#"
        voice {
          graph = function(n)
            assert(n.velocity ~= nil and n.duration ~= nil)
            assert(n.vel == nil and n.gate == nil)
            return sine(n.hz) * n.velocity * (1 + n.duration * 0)
          end,
        }
        "#,
    )
    .unwrap();
}

#[test]
fn typed_patch_operators_lower_song_shaped_graphs() {
    let program = evaluate(
        r#"
        voice {
          graph = function(n)
            local body = n.hz >> sine()
            local click = (noise() | dc(3200) | dc(0.8)) >> highpass()
            return (body + click)
                >> shape("tanh", 1.4)
                >> dcblock()
                >> mul(n.velocity)
                >> pan(n.pan)
          end,
        }
        "#,
    )
    .unwrap();

    let voice = &program.voices[0];
    assert_eq!(voice.channels(), 2);
    for expected in [
        Op::Sine,
        Op::Highpass,
        Op::Add,
        Op::Shape {
            kind: apteronotus_synth::ShapeKind::Tanh,
            amount: 1.4,
        },
        Op::DcBlock,
        Op::Mul,
        Op::Pan,
    ] {
        assert!(voice.nodes.iter().any(|node| node.op == expected));
    }
}

#[test]
fn processors_mix_scale_bus_branch_and_build_dynamic_banks() {
    let program = evaluate(
        r#"
        voice {
          graph = function(n)
            local bank = ring(587, secs(0.3)) + ring(845, secs(0.25)) * 0.7
            return impulse() >> bank >> (pan(-0.5) ~ pan(0.5))
          end,
        }

        voice {
          graph = function(n)
            local modes = {
              ring(n.hz, secs(0.3)),
              ring(n.hz * 2, secs(0.2)) * 0.5,
            }
            return impulse() >> (mix(modes) & ring(n.hz * 3, secs(0.15)))
          end,
        }
        "#,
    )
    .unwrap();

    assert_eq!(program.voices[0].channels(), 4);
    assert_eq!(program.voices[1].channels(), 1);
    assert!(
        program.voices[0]
            .nodes
            .iter()
            .filter(|node| node.op == Op::Bandpass)
            .count()
            >= 2
    );
}

#[test]
fn patch_connections_check_processor_port_arity_before_publication() {
    let program = evaluate(
        r#"
        local ok, message = pcall(function()
          voice {
            graph = function(n)
              return n.hz >> lowpass()
            end,
          }
        end)
        assert(not ok)

        voice {
          graph = function(n)
            return (sine(n.hz) | dc(1200) | dc(0.7)) >> lowpass()
          end,
        }
        "#,
    )
    .unwrap();
    assert_eq!(program.voices.len(), 1);
}

#[test]
fn direct_pattern_calls_receive_call_site_provenance() {
    let program = evaluate(
        r#"
        local instrument = voice {
          graph = function(n) return sine(n.hz) end,
        }
        play(instrument, "c4")
        play(instrument, "c4")
        for i = 1, 2 do
          play(instrument, "d4")
        end
        local first = pattern("e4")
        local second = pattern("e4")
        play(instrument, first)
        play(instrument, second)
        "#,
    )
    .unwrap();

    let seed = |track: usize| program.tracks[track].pattern.onsets(Span::cycle(0))[0].seed();
    assert_ne!(seed(0), seed(1));
    assert_eq!(seed(2), seed(3));
    assert_ne!(seed(4), seed(5));
}

#[test]
fn persistent_patches_controls_buses_and_graph_sends_are_plain_program_data() {
    let program = evaluate(
        r#"
        local field = control {
          name = "field",
          range = { 0, 1 },
          default = 0.6,
        }
        local room = bus { channels = 2 }

        local rack = patch {
          inputs = 1,
          params = {
            gain = { 0, 2, 0.8 },
          },
          graph = function(cv)
            return cv.input >> delay(ms(2)) >> mul(cv.gain * field)
          end,
        }
        run(rack)

        voice {
          graph = function(n)
            return (n.hz >> sine()) >> pan(n.pan) >> to(room, 0.25)
          end,
        }
        "#,
    )
    .unwrap();

    assert_eq!(program.controls.specs().len(), 2);
    assert_eq!(program.patches.len(), 1);
    assert_eq!(program.patches[0].graph().inputs, 1);
    assert_eq!(program.runs[0].index(), 0);
    assert_eq!(program.buses.total_channels(), 4);
    assert_eq!(program.voices[0].sends.len(), 1);
    assert!(
        program.patches[0]
            .graph()
            .nodes
            .iter()
            .any(|node| matches!(node.op, Op::Delay(_)))
    );
}

#[test]
fn note_curves_and_initial_random_lower_without_runtime_lua() {
    let program = evaluate(
        r#"
        voice {
          graph = function(n)
            local drift = init_rand(n.id, "oscillator", -0.01, 0.01)
            local envelope = decay(ms(80))
            return sine(n.hz * (1 + drift)) * envelope
          end,
        }
        "#,
    )
    .unwrap();

    let graph = &program.voices[0];
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, Op::InitRandom { .. }))
    );
    assert!(
        graph
            .nodes
            .iter()
            .any(|node| matches!(node.op, Op::Curve(_)))
    );
}

#[test]
fn publication_budgets_are_distinct_from_construction_budgets() {
    let limits = Limits {
        graph_nodes: 100,
        graph_publication: GraphLimits {
            nodes: 0,
            ..Limits::default().graph_publication
        },
        ..Limits::default()
    };
    let error = Evaluator::new(limits)
        .evaluate(
            r#"
            voice {
              graph = function(n) return sine(n.hz) end,
            }
            "#,
        )
        .unwrap_err();
    assert!(error.to_string().contains("more than the limit of 0"));
}

#[test]
fn tables_can_grow_after_deleting_string_keys() {
    // Regression for a panic in the Piccolo 0.3.3 release. This is why the
    // crate currently pins a newer exact upstream revision.
    evaluate(
        r#"
        local values = { old = true }
        values.old = nil
        for i = 1, 200 do
          values["key-" .. i] = i
        end
        assert(values["key-200"] == 200)
        "#,
    )
    .unwrap();
}

#[test]
fn voice_scoped_setters_carry_note_phase_curves_into_owned_patterns() {
    let program = evaluate(
        r#"
        local pad = voice {
          params = {
            pressure = { 0, 1, 0 },
          },
          graph = function(n)
            return sine(n.hz) * n.pressure
          end,
        }
        local pressure = curve {
          clock = "note_phase",
          offset = 0,
          { basis = "ramp", coefficient = 1, delay = 0, length = 1 },
        }
        local notes = pattern("c4 e4") >> pad.pressure(pressure)
        play(pad, notes)
        "#,
    )
    .unwrap();

    let event = program.tracks[0].pattern.onsets(Span::cycle(0)).remove(0);
    let ControlValue::Curve(curve) = event.value.as_map().unwrap().get("pressure").unwrap() else {
        panic!("pressure was sampled instead of carried as a curve");
    };
    assert_eq!(curve.clock, CurveClock::NotePhase);
}

#[test]
fn play_revalidates_control_compatibility_for_the_target_voice() {
    let error = evaluate(
        r#"
        local wide = voice {
          params = { pressure = { 0, 1, 0 } },
          graph = function(n) return sine(n.hz) * n.pressure end,
        }
        local narrow = voice {
          params = { pressure = { 0, 0.5, 0 } },
          graph = function(n) return sine(n.hz) * n.pressure end,
        }
        local notes = pattern("c4") >> wide.pressure(0.8)
        play(narrow, notes)
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("outside 0..0.5"));
}

#[test]
fn note_is_a_privileged_primary_map_and_leaf_merge_is_rejected() {
    let program = evaluate(
        r#"
        local pad = voice {
          graph = function(n) return sine(n.hz) end,
        }
        assert(not pcall(function()
          return pattern("c4") >> pattern("d4")
        end))
        play(pad, pattern("c4") >> note("d4"))
        "#,
    )
    .unwrap();
    let event = program.tracks[0].pattern.onsets(Span::cycle(0)).remove(0);
    assert_eq!(
        event
            .value
            .as_map()
            .and_then(|map| map.get("value"))
            .and_then(ControlValue::as_str),
        Some("d4")
    );
}

#[test]
fn structural_pattern_transforms_build_owned_ast_nodes() {
    let program = evaluate(
        r#"
        local tone = voice {
          graph = function(n) return sine(n.hz) * n.velocity end,
        }

        play(tone, pattern("c4 d4") >> slow(2) >> shift(bars(0.25)))
        play(tone, pattern("e4 f4") >> every(2, rev))
        play(tone, pattern("g4 a4") >> sometimes(1, rev))
        play(tone, pattern("0 1") >> segment(4) >> range(60, 72))
        "#,
    )
    .unwrap();

    let shifted = program.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(shifted.len(), 1);
    assert_eq!(shifted[0].part.begin, Frac::new(1, 4));
    assert_eq!(shifted[0].value, Value::text("c4"));

    let reversed = program.tracks[1].pattern.onsets(Span::cycle(0));
    let at = |onset| {
        reversed
            .iter()
            .find(|event| event.part.begin == onset)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(at(Frac::ZERO), Value::text("f4"));
    assert_eq!(at(Frac::new(1, 2)), Value::text("e4"));

    let ordinary = program.tracks[1].pattern.onsets(Span::cycle(1));
    let at = |onset| {
        ordinary
            .iter()
            .find(|event| event.part.begin == onset)
            .unwrap()
            .value
            .clone()
    };
    assert_eq!(at(Frac::ONE), Value::text("e4"));
    assert_eq!(at(Frac::new(3, 2)), Value::text("f4"));

    let always_reversed = program.tracks[2].pattern.onsets(Span::cycle(0));
    assert_eq!(
        always_reversed
            .iter()
            .find(|event| event.part.begin == Frac::ZERO)
            .unwrap()
            .value,
        Value::text("a4")
    );

    let segmented = program.tracks[3].pattern.onsets(Span::cycle(0));
    assert_eq!(segmented.len(), 4);
    assert_eq!(segmented[0].value, Value::number(60.0));
    assert_eq!(segmented[1].value, Value::number(60.0));
    assert_eq!(segmented[2].value, Value::number(72.0));
    assert_eq!(segmented[3].value, Value::number(72.0));
}

#[test]
fn degrade_uses_stable_distinct_source_site_seeds() {
    let source = r#"
        local tone = voice {
          graph = function(n) return sine(n.hz) end,
        }
        play(tone, pattern("c4*8") >> degrade(0.5))
        play(tone, pattern("c4*8") >> degrade(0.5))
    "#;
    let first = evaluate(source).unwrap();
    let second = evaluate(source).unwrap();

    let seed = |program: &apteronotus_lua::Program, track: usize| {
        let apteronotus_pattern::Pattern::Degrade { seed, .. } = &program.tracks[track].pattern
        else {
            panic!("degrade did not build a pattern AST node");
        };
        *seed
    };
    assert_ne!(seed(&first, 0), seed(&first, 1));
    assert_eq!(seed(&first, 0), seed(&second, 0));
    assert_eq!(seed(&first, 1), seed(&second, 1));
}

#[test]
fn compatible_persistent_programs_rebind_new_voices_to_the_live_arena() {
    let source = |oscillator: &str, drone_hz: f64, inert_patch: &str| {
        format!(
            r#"
            local level = control {{
              name = "level",
              range = {{ 0, 1 }},
              default = 0.2,
            }}
            local room = bus {{ channels = 1 }}
            local drone = patch {{
              graph = function()
                return ((sine({drone_hz}) * level) >> to(room, 0.2)) >> pan(0)
              end,
            }}
            run(drone)
            {inert_patch}
            local voice = voice {{
              graph = function(n)
                return (({oscillator}(n.hz) * level) >> to(room, 0.3)) >> pan(0)
              end,
            }}
            play(voice, "c4")
            "#
        )
    };

    let active = evaluate(&source("sine", 110.0, "")).unwrap();
    let mut compatible = evaluate(&source(
        "saw",
        110.0,
        "local spare = patch { graph = function() return sine(333) >> pan(0) end }",
    ))
    .unwrap();
    assert!(compatible.persistent_compatible_with(&active));
    assert!(compatible.reuse_persistent_from(&active));

    let voice = &compatible.voices[0];
    assert!(
        voice
            .sends
            .iter()
            .all(|send| compatible.buses.bus_channels(send.bus).is_some())
    );
    assert!(
        voice
            .nodes
            .iter()
            .flat_map(|node| &node.inputs)
            .all(|input| {
                let apteronotus_synth::Source::Control(id) = input.source else {
                    return true;
                };
                compatible.controls.spec(id).is_some()
            })
    );
    assert!(
        voice
            .nodes
            .iter()
            .any(|node| node.op == apteronotus_synth::Op::Saw)
    );
    assert_eq!(compatible.patches.len(), 2);
    assert_eq!(compatible.patches[0], active.patches[0]);

    let changed_patch = evaluate(&source("saw", 111.0, "")).unwrap();
    assert!(!changed_patch.persistent_compatible_with(&active));
}
