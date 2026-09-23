use apteronotus_lua::{EvalError, Evaluator, Limits, evaluate};
use apteronotus_pattern::{ControlValue, CurveClock, Frac, Span, Value};
use apteronotus_synth::{
    ControlStore, GraphLimits, Note, Op, Source, instantiate, instantiate_patch,
    instantiate_with_controls,
    lower::{render, rms, zero_crossing_hz},
};

const SR: f64 = 48_000.0;

#[test]
fn string_resonator_exposes_modulatable_inputs_and_rejects_bad_bounds() {
    let program = evaluate(
        r#"
        local p = patch { inputs = 1, graph = function(cv)
            return string_resonator(cv.input, 220 + sine(5) * 3, 0,
                { min_hz = 40, decay = secs(20) }) >> pan(0)
        end }
        run(p)
    "#,
    )
    .unwrap();
    assert!(program.patches[0].graph().nodes.iter().any(|n| matches!(
        n.op,
        Op::StringResonator {
            min_hz: 40.0,
            decay: 20.0
        }
    )));
    for bounds in [
        "{min_hz=0, decay=secs(20)}",
        "{min_hz=40, decay=secs(0)}",
        "{min_hz=40, decay=secs(121)}",
    ] {
        let source = format!(
            "patch {{ graph = function() return string_resonator(0, 220, 0, {bounds}) end }}"
        );
        assert!(
            evaluate(&source)
                .unwrap_err()
                .to_string()
                .contains("string_resonator")
        );
    }
    assert!(evaluate("string_resonator(0, 220, 0, {min_hz=40, decay=secs(20)})").is_err());
}

#[test]
fn triangle_supports_direct_modulation_and_processor_forms() {
    let program = evaluate(
        r#"
      local direct = voice { graph = function(n)
        return triangle(n.hz + sine(3) * 2) * 0.1 >> pan(0)
      end }
      local piped = voice { graph = function(n)
        return n.hz >> triangle() >> mul(0.1) >> pan(0)
      end }
      play(direct, "a3")
      play(piped, "a3")
    "#,
    )
    .unwrap();
    for graph in &program.voices {
        assert_eq!(
            graph
                .nodes
                .iter()
                .filter(|node| node.op == Op::Triangle)
                .count(),
            1
        );
        let audio = render(
            instantiate(graph, &Note::new(220.0)).unwrap().as_mut(),
            SR,
            0.2,
        );
        assert!(rms(&audio[0]) > 0.01);
        assert!((zero_crossing_hz(&audio[0], SR) - 220.0).abs() < 8.0);
    }
    assert!(evaluate("triangle(220)").is_err());
    assert!(evaluate("voice { graph = function() return triangle(220, 440) end }").is_err());
}

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
fn mini_event_spans_slice_the_original_lua_document() {
    let source = r#"
local tone = voice { graph = function() return sine(220) * 0.01 end }
play(tone, "c4 e4")
"#;
    let program = evaluate(source).unwrap();
    let mut events = program.tracks[0].pattern.onsets(Span::cycle(0));
    events.sort_by_key(|event| event.src.unwrap().start);
    let slices = events
        .iter()
        .map(|event| {
            let span = event.src.unwrap();
            &source[span.start as usize..span.end as usize]
        })
        .collect::<Vec<_>>();

    assert_eq!(slices, ["c4", "e4"]);
}

#[test]
fn timeline_placement_preserves_document_absolute_mini_spans() {
    let source = r#"
local tone = voice { graph = function() return sine(220) * 0.01 end }
local score = timeline { at(bars(2), pattern("g4")) }
play(tone, score)
"#;
    let program = evaluate(source).unwrap();
    let event = &program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::int(2), Frac::int(3)))[0];
    let span = event.src.unwrap();

    assert_eq!(&source[span.start as usize..span.end as usize], "g4");
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

    let beat_program = evaluate(
        r#"
        tempo(120)
        local timed = voice {
          graph = function(n) return sine(n.hz) >> delay(beats(1)) end,
        }
        play(timed, pattern("c4") >> shift(beats(1)))
        "#,
    )
    .unwrap();
    assert_eq!(
        beat_program.tracks[0].pattern.onsets(Span::cycle(0))[0]
            .part
            .begin,
        Frac::new(1, 4)
    );

    let beat_order_error = evaluate("local x = beats(1)\ntempo(120)")
        .unwrap_err()
        .to_string();
    assert!(beat_order_error.contains("requires tempo"));

    let changing_beat_error = evaluate(
        r#"
        tempo {
          { at = bars(0), bpm = 120 },
          { at = bars(4), bpm = 90 },
        }
        local timed = voice {
          graph = function(n) return sine(n.hz) >> delay(beats(1)) end,
        }
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(changing_beat_error.contains("changing tempo"));

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
fn optional_audio_input_count_is_host_bounded() {
    let error = Evaluator::new(Limits {
        audio_inputs: 0,
        ..Limits::default()
    })
    .evaluate(
        r#"
        local room = audio_input {
          name = "room",
          channels = 1,
          fallback = "silence",
        }
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("audio input limit of 0 exceeded"));
}

#[test]
fn cycle_hold_lookback_has_its_own_host_policy_limit() {
    let limits = Limits {
        max_hold_cycles: 4,
        ..Limits::default()
    };
    for source in [
        r#"local notes = pattern("c4") >> hold(bars(5))"#,
        r#"tempo(120); local notes = pattern("c4") >> hold(beats(17))"#,
    ] {
        let error = Evaluator::new(limits)
            .evaluate(source)
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("query look-back limit of 4 cycles"),
            "{error}"
        );
    }

    // Seconds holds become finite timeline event extents. They do not create a
    // Pattern::Hold look-back and therefore use a different resource policy.
    Evaluator::new(Limits {
        max_hold_cycles: 0,
        ..Limits::default()
    })
    .evaluate(
        r#"
        tempo(120)
        local score = timeline {
          at(bars(0), pattern("c4") >> hold(secs(1))),
        }
        "#,
    )
    .unwrap();
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
        },
        Op::DcBlock,
        Op::Mul,
        Op::Pan,
    ] {
        assert!(voice.nodes.iter().any(|node| node.op == expected));
    }
}

#[test]
fn zero_and_soft_saw_cover_accumulator_and_readable_stdlib_roles() {
    let program = evaluate(
        r#"
        local pad = voice {
          graph = function(n)
            local oscillators = zero()
            for i = -1, 1 do
              oscillators = oscillators + soft_saw(n.hz * (1 + i * 0.003))
            end
            return oscillators * 0.02
          end,
        }
        play(pad, "c4")
        "#,
    )
    .unwrap();

    assert!(program.voices[0].nodes.len() >= 15);
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
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
    assert_eq!(program.runs[0].patch.index(), 0);
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
fn user_controls_cannot_enter_the_engine_owned_signal_namespace() {
    let error = evaluate(
        r#"
        local hidden = control {
          name = "__apteronotus.signal.0",
          range = { 0, 1 },
          default = 0.5,
        }
        "#,
    )
    .unwrap_err();
    assert!(error.to_string().contains("are reserved"), "{error}");
}

#[test]
fn score_sends_are_owned_track_routing_not_graph_taps() {
    let program = evaluate(
        r#"
        local room = bus { channels = 2 }
        local pad = voice {
          graph = function(n)
            return sine(n.hz) * 0.05 >> pan(n.pan)
          end,
        }
        play(pad, pattern("c4 e4") >> to(room, 0.35))
        "#,
    )
    .unwrap();

    assert!(program.voices[0].sends.is_empty());
    assert_eq!(program.tracks[0].routing.sends().len(), 1);
    assert_eq!(program.tracks[0].routing.sends()[0].level, 0.35);
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
}

#[test]
fn program_send_returns_and_stereo_master_finalize_after_bus_layout() {
    let program = evaluate(
        r#"
        tempo(120)
        local room = send {
          graph = reverb(18, secs(1.2), 0.55),
          level = 0.25,
        }
        local tone = voice {
          graph = function(n)
            return (sine(n.hz) * decay(ms(80)) * 0.08)
                >> pan(n.pan)
                >> to(room, 0.3)
          end,
        }
        play(tone, "c4 ~")
        master(limiter(ms(3), ms(120)) >> mul(0.85))
        "#,
    )
    .unwrap();

    assert_eq!(program.runs.len(), 1);
    let patch = program.patch(program.runs[0].patch).unwrap();
    assert_eq!(patch.graph().inputs, program.buses.total_channels());
    assert_eq!(patch.graph().channels(), program.buses.main_channels());
}

#[test]
fn diffuser_spellings_expand_to_bounded_allpass_stages() {
    let program = evaluate(
        r#"
        local compact = voice {
          graph = function(n)
            return sine(n.hz) >> diffuse({ ms(4.7), ms(6.8) }, 0.72)
          end,
        }
        local explicit = voice {
          graph = function(n)
            return sine(n.hz) >> diffuse {
              delays = { ms(10.1), ms(13.7) },
              gains = { 0.67, -0.65 },
            }
          end,
        }
        play(compact, "c4")
        play(explicit, "e4")
        "#,
    )
    .unwrap();

    for voice in &program.voices {
        let stages = voice
            .nodes
            .iter()
            .filter(|node| matches!(node.op, Op::AllpassDelay { .. }))
            .count();
        assert_eq!(stages, 2);
    }
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
}

#[test]
fn send_fdn_resolves_its_declared_live_decay_control() {
    let program = evaluate(
        r#"
        local hall = send {
          params = {
            decay = { 1, 18, 8.6, "s" },
          },
          graph = diffuse({ ms(4.7), ms(6.8) }, 0.7)
               >> fdn {
                    delays = { ms(43.7), ms(47.9), ms(53.3), ms(59.9) },
                    decay = param("decay"),
                    damping = 0.46,
                    modulation = { rate = 0.11, depth = ms(1.7) },
                  },
          level = 0.4,
        }
        local tone = voice {
          graph = function(n)
            return sine(n.hz) * decay(ms(60)) >> pan(0) >> to(hall, 0.5)
          end,
        }
        play(tone, "c4")
        "#,
    )
    .unwrap();

    assert!(program.controls.specs().iter().any(|control| {
        control.name == "send1.decay"
            && control.min == 1.0
            && control.max == 18.0
            && control.default == 8.6
    }));
    let return_patch = program.patch(program.runs[0].patch).unwrap();
    assert!(
        return_patch
            .graph()
            .nodes
            .iter()
            .any(|node| { matches!(node.op, Op::Fdn { max_t60: 18.0, .. }) })
    );
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
}

#[test]
fn finite_patch_play_builds_a_control_driver_sharing_the_patch_arena() {
    let program = evaluate(
        r#"
        tempo(120)
        local lead = patch {
          controls = {
            pitch = note_control("c4"),
            gate = gate_control(false),
            pressure = control { range = { 0, 1 }, default = 0.4 },
          },
          graph = function(c)
            return sine(note_hz(c.pitch))
              * gate_env(c.gate, ms(2), ms(10), 0.8, ms(30))
              * c.pressure
          end,
        }
        play(lead, timeline {
          at(bars(0), note("c4") >> hold(secs(0.2)) >> lead.pressure(0.7))
        })
        "#,
    )
    .unwrap();

    let controls = ControlStore::new(&program.controls);
    let patch_controls = &program.patch_controls[0];
    let gate = patch_controls
        .iter()
        .find(|control| control.name == "gate")
        .unwrap()
        .id;
    let driver = program.voice(program.tracks[0].voice).unwrap();
    let mut driver =
        instantiate_with_controls(driver, &Note::new(261.6256).duration(0.2), &controls).unwrap();
    let mut patch = instantiate_patch(&program.patches[0], &controls).unwrap();
    driver.set_sample_rate(SR);
    patch.set_sample_rate(SR);
    let mut driver_frame = vec![0.0; driver.outputs()];
    let mut patch_frame = vec![0.0; patch.outputs()];
    let mut peak = 0.0_f32;
    for _ in 0..2_000 {
        driver.tick(&[], &mut driver_frame);
        patch.tick(&[], &mut patch_frame);
        peak = peak.max(patch_frame[0].abs());
    }
    assert_eq!(controls.value(gate).unwrap(), 1.0);
    assert!(peak > 0.01);
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
        local notes = pattern("c4 e4") >> hold(bars(1)) >> pad.pressure(pressure)
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
fn phase_breakpoints_desugar_to_a_live_curve_and_require_hold() {
    let program = evaluate(
        r#"
        local pad = voice {
          params = { pressure = { 0, 1, 0 } },
          graph = function(n) return sine(n.hz) * n.pressure end,
        }
        local shape = curve {
          { phase(0.00), 0.10 },
          { phase(0.25), 0.90 },
          { phase(1.00), 0.20 },
        }
        play(pad, pattern("c4") >> hold(bars(2)) >> pad.pressure(shape))
        "#,
    )
    .unwrap();

    let event = program.tracks[0].pattern.onsets(Span::cycle(0)).remove(0);
    let ControlValue::Curve(curve) = event.value.as_map().unwrap().get("pressure").unwrap() else {
        panic!("breakpoint curve was sampled instead of carried whole");
    };
    assert_eq!(curve.clock, CurveClock::NotePhase);
    assert_eq!(curve.terms.len(), 2);
    assert!((curve.at_coordinate(0.25) - 0.90).abs() < 1.0e-12);
    assert!((curve.at_coordinate(1.00) - 0.20).abs() < 1.0e-12);

    let error = evaluate(
        r#"
        local pad = voice {
          params = { pressure = { 0, 1, 0 } },
          graph = function(n) return sine(n.hz) * n.pressure end,
        }
        local shape = curve {
          { phase(0), 0 },
          { phase(1), 1 },
        }
        play(pad, pattern("c4") >> pad.pressure(shape))
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("require an explicit hold"), "{error}");
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
fn arp_serializes_group_members_with_derived_or_explicit_spacing() {
    let program = evaluate(
        r#"
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.03 end,
        }
        play(v, pattern("[c4,e4,g4]") >> arp("down"))
        "#,
    )
    .unwrap();
    let events = program.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(
        events
            .iter()
            .map(|event| event.whole.unwrap().begin)
            .collect::<Vec<_>>(),
        vec![Frac::ZERO, Frac::new(1, 3), Frac::new(2, 3)]
    );

    let spaced = evaluate(
        r#"
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.03 end,
        }
        play(v, pattern("[c4,e4,g4]") >> arp("up", bars(0.25)))
        "#,
    )
    .unwrap();
    let events = spaced.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(
        events
            .iter()
            .map(|event| event.whole.unwrap())
            .collect::<Vec<_>>(),
        vec![
            Span::new(Frac::ZERO, Frac::new(1, 4)),
            Span::new(Frac::new(1, 4), Frac::new(1, 2)),
            Span::new(Frac::new(1, 2), Frac::new(3, 4)),
        ]
    );
}

#[test]
fn literal_chords_expand_to_grouped_owned_pitches_before_query_time() {
    let program = evaluate(
        r#"
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.03 end,
        }
        local voiced = chord("Dm(add9)") >> anchor("a4") >> voicing("open-5")
        play(v, voiced)
        play(v, voiced >> arp("outside-in"))
        local finite = timeline { at(bars(0), voiced) }
        play(v, finite >> root_notes() >> octave(-2))
        "#,
    )
    .unwrap();

    let chord = program.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(chord.len(), 5);
    for (index, event) in chord.iter().enumerate() {
        let group = event.group.unwrap();
        assert_eq!(group.index, index as u32);
        assert_eq!(group.count, 5);
        assert!(event.src.is_some());
        let midi = event
            .value
            .as_map()
            .unwrap()
            .get("value")
            .unwrap()
            .as_f64()
            .unwrap();
        assert!([2, 4, 5, 9].contains(&((midi.round() as i32).rem_euclid(12))));
        assert!(midi <= 69.0);
    }

    let arp = program.tracks[1].pattern.onsets(Span::cycle(0));
    assert_eq!(arp.len(), 5);
    assert!(arp.iter().all(|event| event.group.is_none()));
    assert_eq!(
        arp.iter()
            .map(|event| event.whole.unwrap().begin)
            .collect::<Vec<_>>(),
        vec![
            Frac::ZERO,
            Frac::new(1, 5),
            Frac::new(2, 5),
            Frac::new(3, 5),
            Frac::new(4, 5),
        ]
    );

    let root = program.tracks[2].pattern.onsets(Span::cycle(0));
    assert_eq!(root.len(), 1);
    assert!(root[0].group.is_none());
    let midi = root[0]
        .value
        .as_map()
        .unwrap()
        .get("value")
        .unwrap()
        .as_f64()
        .unwrap();
    assert_eq!((midi.round() as i32).rem_euclid(12), 2);
    assert!(midi < 48.0);

    let error = evaluate(
        r#"
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.03 end,
        }
        play(v, pattern("c4") >> octave(-1))
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("numeric event values"), "{error}");
}

#[test]
fn tempo_and_timeline_evaluate_to_owned_finite_score_data() {
    let program = evaluate(
        r#"
        tempo {
          { at = bars(0), bpm = 60 },
          { at = bars(2), bpm = 120 },
        }
        local tone = voice {
          graph = function(n) return sine(n.hz) * n.velocity * 0.08 end,
        }
        local score = timeline {
          at(bars(0), pattern("[c4,e4,g4]")),
          at(secs(9), pattern("a4")),
        }
        play(tone, score)
        "#,
    )
    .unwrap();

    assert_eq!(program.tempo.bpm_at(Frac::ZERO), 60.0);
    assert_eq!(program.tempo.bpm_at(Frac::int(2)), 120.0);
    let events = program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::ZERO, Frac::int(8)));
    assert_eq!(events.len(), 4);
    assert_eq!(events[3].whole.unwrap().begin, Frac::new(5, 2));
    assert_eq!(
        events[0].group.map(|group| (group.index, group.count)),
        Some((0, 3))
    );
    assert!(
        program.tracks[0]
            .pattern
            .query(Span::new(Frac::int(4), Frac::int(8)))
            .is_empty()
    );
}

#[test]
fn hold_uses_exact_cycle_time_or_resolves_seconds_at_timeline_placement() {
    let cycle_program = evaluate(
        r#"
        local tone = voice {
          graph = function(n) return sine(n.hz) * 0.01 end,
        }
        play(tone, pattern("c4 e4") >> hold(bars(2)))
        "#,
    )
    .unwrap();
    let cycle_events = cycle_program.tracks[0].pattern.onsets(Span::cycle(0));
    assert_eq!(cycle_events[0].whole.unwrap().length(), Frac::int(2));
    assert_eq!(cycle_events[1].whole.unwrap().length(), Frac::int(2));

    let seconds_program = evaluate(
        r#"
        tempo {
          { at = bars(0), bpm = 60 },
          { at = bars(2), bpm = 120, over = bars(2) },
        }
        local tone = voice {
          graph = function(n) return sine(n.hz) * 0.01 end,
        }
        play(tone, timeline {
          at(bars(0), pattern("c4") >> hold(secs(1))),
          at(bars(1), pattern("e4") >> hold(secs(1))),
        })
        "#,
    )
    .unwrap();
    let events = seconds_program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::ZERO, Frac::int(3)));
    assert_eq!(events.len(), 2);
    for event in events {
        let whole = event.whole.unwrap();
        let seconds = seconds_program.tempo.span_to_seconds(whole);
        assert!((seconds - 1.0).abs() < 1.0e-5, "{whole:?} lasted {seconds}");
    }

    let error = evaluate(
        r#"
        tempo(120)
        local tone = voice {
          graph = function(n) return sine(n.hz) * 0.01 end,
        }
        play(tone, pattern("c4") >> hold(secs(1)))
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(error.contains("must be resolved inside at"), "{error}");
}

#[test]
fn key_is_owned_tonal_context_not_an_evaluation_noop() {
    let program = evaluate(
        r#"
        key("f#", "minor")
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.02 end,
        }
        play(v, "f#3")
        "#,
    )
    .unwrap();

    let key = program.key.expect("key declaration is owned by Program");
    assert_eq!(key.tonic.semitones(), 6);
    assert_eq!(key.mode, apteronotus_music::Mode::Minor);
    assert!(
        evaluate("key('d', 'dorian'); key('c', 'major')")
            .unwrap_err()
            .to_string()
            .contains("only once")
    );
}

#[test]
fn absolute_timeline_placements_require_an_explicit_prior_clock() {
    let missing = evaluate(
        r#"
        local score = timeline { at(secs(1), pattern("c4")) }
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(missing.contains("requires tempo(...) earlier"));

    let late = evaluate(
        r#"
        local score = timeline { at(secs(1), pattern("c4")) }
        tempo(120)
        "#,
    )
    .unwrap_err()
    .to_string();
    assert!(late.contains("requires tempo(...) earlier"));
}

#[test]
fn transport_signals_build_owned_arithmetic_and_dynamic_setters() {
    let program = evaluate(
        r#"
        local pad = voice {
          params = { cutoff = { 200, 2000, 800, "hz" } },
          graph = function(n)
            return sine(n.hz) * n.velocity * 0.08
          end,
        }
        local main = step(bars(1))
        local movement = scale(400, 1600, sine(0.5))
        play(pad, pattern("c4*4")
          >> velocity("0.5 1" * (0.5 + 0.5 * main))
          >> pad.cutoff(movement)
          >> pan("-0.25 0.25"))
        "#,
    )
    .unwrap();

    let first = program.tracks[0].pattern.onsets(Span::cycle(0));
    let second = program.tracks[0].pattern.onsets(Span::cycle(1));
    let field = |event: &apteronotus_pattern::Event, name| {
        event
            .value
            .as_map()
            .unwrap()
            .get(name)
            .and_then(ControlValue::as_f64)
            .unwrap()
    };
    assert_eq!(field(&first[0], "velocity"), 0.25);
    assert_eq!(field(&first[2], "velocity"), 0.5);
    assert_eq!(field(&second[0], "velocity"), 0.5);
    assert_eq!(field(&second[2], "velocity"), 1.0);
    assert!((400.0..=1600.0).contains(&field(&first[0], "cutoff")));
    assert!(
        first[0]
            .value
            .as_map()
            .unwrap()
            .field("cutoff")
            .unwrap()
            .src()
            .is_some()
    );
    assert_eq!(field(&first[0], "pan"), -0.25);
    assert_eq!(field(&first[2], "pan"), 0.25);
}

#[test]
fn pattern_arithmetic_rejects_text_instead_of_silently_preserving_it() {
    let error = evaluate(
        r#"
        local v = voice {
          graph = function(n) return sine(n.hz) * 0.05 end,
        }
        local movement = step(bars(1))
        play(v, pattern("c4 e4") * movement)
        "#,
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("pattern arithmetic requires numeric event values"));
    assert!(error.contains("source bytes"));
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

#[test]
fn optional_inputs_are_owned_once_and_can_feed_multiple_graphs() {
    let program = evaluate(
        r#"
        local city = audio_input {
          name = "city",
          channels = 1,
          fallback = "silence",
        }
        local expression = control_input {
          name = "expression",
          range = { 0, 1 },
          default = 0.4,
        }
        local first = patch {
          graph = function()
            return (city + expression * 0) >> pan(-0.2)
          end,
        }
        local second = patch {
          graph = function()
            return (city * 0.5) >> pan(0.2)
          end,
        }
        run(first)
        run(second)
        "#,
    )
    .unwrap();

    assert_eq!(program.audio_inputs.specs().len(), 1);
    assert_eq!(program.audio_inputs.specs()[0].name, "city");
    assert_eq!(program.audio_inputs.specs()[0].channels, 1);
    assert_eq!(program.control_inputs, ["expression"]);
    assert!(program.controls.id("expression").is_some());
    for patch in &program.patches {
        assert!(patch.graph().nodes.iter().any(|node| {
            node.inputs
                .iter()
                .any(|input| matches!(input.source, Source::ExternalAudio { .. }))
        }));
    }
}

#[test]
fn compatible_optional_inputs_rebind_across_evaluation_arenas() {
    let source = r#"
        local city = audio_input {
          name = "city",
          channels = 1,
          fallback = "silence",
        }
        local expression = control_input {
          name = "expression",
          range = { 0, 1 },
          default = 0.4,
        }
        local rack = patch {
          graph = function()
            return (city + expression * 0) >> pan(0)
          end,
        }
        run(rack)
    "#;
    let active = evaluate(source).unwrap();
    let mut candidate = evaluate(source).unwrap();

    assert!(candidate.persistent_compatible_with(&active));
    assert!(candidate.reuse_persistent_from(&active));
    candidate
        .validate(Limits::default().graph_publication)
        .unwrap();

    let changed = evaluate(&source.replace("name = \"city\"", "name = \"street\"")).unwrap();
    assert!(!changed.persistent_compatible_with(&active));
}

#[test]
fn finite_run_owns_an_exact_transport_span() {
    let program = evaluate(
        r#"
        tempo(120)
        local weather = patch {
          graph = function()
            return sine(110) * 0.05 >> pan(0)
          end,
        }
        run(weather, span(bars(1), bars(3)))
        "#,
    )
    .unwrap();

    assert_eq!(program.runs.len(), 1);
    assert_eq!(
        program.runs[0].span,
        Some(apteronotus_pattern::Span::new(
            apteronotus_pattern::Frac::ONE,
            apteronotus_pattern::Frac::int(3)
        ))
    );
}

#[test]
fn transport_pattern_control_is_compiled_into_flat_persistent_data() {
    let program = evaluate(
        r#"
        tempo(72)
        local rack = patch {
          graph = function()
            local pulse = control_signal(
              "1 0.2 0.6 0.15" >> segment(4) >> slow(3),
              bars(3))
            return sine(220) * pulse * 0.05 >> pan(0)
          end,
        }
        run(rack)
        "#,
    )
    .unwrap();

    let graph = program.patch(program.runs[0].patch).unwrap().graph();
    assert!(graph.nodes.iter().any(|node| {
        matches!(
            &node.op,
            Op::TransportSequence {
                period_seconds,
                slots,
            } if (*period_seconds - 10.0).abs() < 1.0e-9 && slots.len() == 4
        )
    }));
}

#[test]
fn transport_pattern_control_period_is_bounded_before_querying() {
    let error = Evaluator::new(Limits {
        max_control_signal_cycles: 0,
        ..Limits::default()
    })
    .evaluate(
        r#"
        tempo(72)
        local rack = patch {
          graph = function()
            return sine(220) * control_signal(pattern("1"), bars(1)) * 0.05 >> pan(0)
          end,
        }
        run(rack)
        "#,
    )
    .unwrap_err();

    assert!(
        error.to_string().contains("compile-window limit"),
        "{error}"
    );
}

#[test]
fn external_onset_and_init_samples_remain_distinct_in_the_owned_track() {
    let program = evaluate(
        r#"
        tempo(72)
        local them = audio_input {
          name = "them",
          channels = 1,
          fallback = "silence",
        }
        local hz = them >> pitch_tracker { min = 65, max = 1100, hold = ms(140) }
        local amp = them >> envelope_follower(ms(6), ms(320))
        local hit = them >> onset_detector { floor = 0.04, hold = ms(90) }
        local bell = voice {
          params = { ring = { 0.4, 6.0, 2.4, "s" } },
          graph = function(n)
            return sine(n.hz) * n.velocity * n.ring * 0.01 >> pan(0)
          end,
        }
        play(bell, hit
          >> bell.hz(at_onset(hz >> slew(ms(20))))
          >> bell.velocity(at_onset(scale(0.15, 0.9, amp)))
          >> bell.ring(at_onset(scale(1.2, 5.0, amp))))
        "#,
    )
    .unwrap();

    let track = &program.tracks[0];
    assert!(track.external_trigger.is_some());
    assert_eq!(track.onset_bindings.len(), 3);
    assert!(track.pattern.query(Span::cycle(0)).is_empty());
    program
        .validate(Limits::default().graph_publication)
        .unwrap();
}

#[test]
fn a_typed_capture_window_keeps_seven_eighths_from_repeating_its_first_note() {
    let program = evaluate(
        r#"
        local v = voice {graph = function(n) return sine(n.hz) * 0.1 end}
        local cell = pattern("c4 d4 e4 f4 g4 a4 b4") >> slow(7 / 8)
        play(v, timeline { at(bars(2), cell, bars(7 / 8)) })
        play(v, timeline { at(bars(2), cell) })
    "#,
    )
    .unwrap();
    let events = program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::ZERO, Frac::int(4)));
    assert_eq!(events.len(), 7);
    for (index, event) in events.iter().enumerate() {
        assert_eq!(
            event.whole.unwrap().begin,
            Frac::int(2) + Frac::new(index as i64, 8)
        );
        assert!(
            event.src.is_some(),
            "capture must preserve source attribution"
        );
    }
    assert_eq!(
        program.tracks[1]
            .pattern
            .onsets(Span::new(Frac::ZERO, Frac::int(4)))
            .len(),
        8,
        "the existing two-argument form still captures exactly one cycle"
    );
}

#[test]
fn longer_captures_step_alternation_and_retain_release_beyond_the_window() {
    let program = evaluate(
        r#"
        local v = voice {graph = function(n) return sine(n.hz) * 0.1 end}
        play(v, timeline { at(bars(4), pattern("<c4 d4>") >> hold(bars(2)), bars(3)) })
    "#,
    )
    .unwrap();
    let events = program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::ZERO, Frac::int(10)));
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].value, events[2].value);
    assert_ne!(events[0].value, events[1].value);
    assert_eq!(
        events[2].whole.unwrap(),
        Span::new(Frac::int(6), Frac::int(8))
    );
    let mut sliced = program.tracks[0]
        .pattern
        .onsets(Span::new(Frac::ZERO, Frac::new(37, 7)));
    sliced.extend(
        program.tracks[0]
            .pattern
            .onsets(Span::new(Frac::new(37, 7), Frac::int(10))),
    );
    assert_eq!(
        events
            .iter()
            .map(|event| (event.whole, &event.value))
            .collect::<Vec<_>>(),
        sliced
            .iter()
            .map(|event| (event.whole, &event.value))
            .collect::<Vec<_>>()
    );
}

#[test]
fn seconds_capture_is_projected_from_its_placement_through_the_tempo_map() {
    let program = evaluate(
        r#"
        tempo { {at = bars(0), bpm = 60}, {at = bars(1), bpm = 120} }
        local v = voice {graph = function(n) return sine(n.hz) * 0.1 end}
        play(v, timeline { at(bars(0), pattern("c4*4"), secs(5)) })
        play(v, timeline { at(bars(1), pattern("c4*4"), secs(1)) })
    "#,
    )
    .unwrap();
    let query = Span::new(Frac::ZERO, Frac::int(4));
    let crossing = program.tracks[0].pattern.onsets(query);
    let later = program.tracks[1].pattern.onsets(query);
    assert_eq!(crossing.len(), 6);
    assert_eq!(later.len(), 2);
    assert_eq!(later[0].whole.unwrap().begin, Frac::ONE);
    assert_eq!(later[1].whole.unwrap().begin, Frac::new(5, 4));
}

#[test]
fn capture_windows_are_typed_positive_and_bounded_before_querying() {
    for duration in ["0.5", "bars(0)", "bars(-1)", "bars(1e20)", "secs(0)"] {
        let source = format!("tempo(120); timeline {{ at(bars(0), pattern('c4'), {duration}) }}");
        assert!(evaluate(&source).is_err(), "{duration}");
    }
    let no_clock = evaluate("timeline { at(bars(0), pattern('c4'), secs(1)) }")
        .unwrap_err()
        .to_string();
    assert!(no_clock.contains("tempo"));
    let small = Evaluator::new(Limits {
        max_timeline_capture_cycles: 2,
        ..Limits::default()
    });
    assert!(
        small
            .evaluate("timeline { at(bars(0), pattern('c4'), bars(2)) }")
            .is_ok()
    );
    assert!(
        small
            .evaluate("timeline { at(bars(0), pattern('c4'), bars(3)) }")
            .unwrap_err()
            .to_string()
            .contains("2 cycles")
    );
    let budget = Evaluator::new(Limits {
        pattern_nodes: 100,
        ..Limits::default()
    });
    let error = budget
        .evaluate("timeline { at(bars(0), pattern('c4*8'), bars(20)) }")
        .unwrap_err()
        .to_string();
    assert!(error.contains("timeline capture"), "{error}");
    assert!(evaluate("at(bars(0), pattern('c4'), bars(1), bars(2))").is_err());
}

#[test]
fn an_unrepresentable_placement_becomes_a_diagnostic_instead_of_panicking() {
    let error = evaluate("timeline { at(bars(1e30), pattern('c4')) }")
        .unwrap_err()
        .to_string();
    assert!(error.contains("cycle-time representation"), "{error}");
}
