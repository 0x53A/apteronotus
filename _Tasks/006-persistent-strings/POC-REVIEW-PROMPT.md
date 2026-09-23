Please review this minimal implementation plan for Apteronotus. Give a concise
opinion with blocking issues, necessary tests, and a recommendation; do not edit
files. This is a Rust/fundsp synthesizer with owned Lua-staged graph data,
persistent patches consuming routed buses, and sample-timed short note voices.
No Lua may execute at audio rate. The user wants a playable sample-free electric
guitar PoC, not a full physical simulation or instrument API framework.

Plan:
- Add one generic `string_resonator(excitation, hz, mute, {min_hz, decay})`
  graph primitive. Three modulatable inputs, one output; allocation min Hz and
  nominal T60 are fixed at staging. It works in a retained patch, not only notes.
- DSP: zero-initialized bounded delay loop, linear fractional interpolation,
  mild two-tap lowpass in feedback, period-calibrated loop loss based on nominal
  T60. Account approximately for lowpass phase delay. Additional interpolation
  and brightness losses mean actual upper-partial decay is shorter. Do not
  claim exact calibrated physical material or T60. Pitch is smoothed briefly;
  clamp frequency to declared minimum and a safe sample-rate-dependent maximum.
- A smoothed 0..1 mute input increases loop loss, so it removes stored energy,
  rather than hiding it behind a VCA. With zero input, pitch changes never
  intentionally clear the delay or create another resonator. No bow, contact
  solver, exact fret collision, energy-preserving retuning or acoustic feedback.
- Input excitation is real scheduled audio, not a boolean edge: tiny filtered
  noise pick voices sent to six separate mono buses. Two identical picks remain
  distinct, and even overlapping picks add excitation to the same string.
- One persistent input-bearing patch consumes main stereo plus six mono buses,
  runs six string nodes, sums them, and applies shared amp distortion/filtering
  to stereo output. Main dry pick output is silent. Short pick voices retire
  promptly while the six string loops remain for the entire performance.
- Existing `control_signal` compiles numeric patterns to a persistent transport
  sequence. Demo pitch/bend and mute trajectories use that plus smoothing;
  live control faders can add per-string bend/mute. No new scheduled command
  API for this PoC. The more convenient guitar/gesture API stays a TODO.
- Add validation, allocation-cost and tail metadata; route only excitation as
  lifetime-bearing. Finite voice use retains nominal decay as response tail;
  persistent processor lifetime is explicit. All allocations outside callbacks.
- Demo: strum all six, wait, repick only upper three while lower strings ring,
  bend one string and repick while bent, release bend, mute, then another chord.
- Tests: silence without excitation; sustained energy after ten seconds;
  repeated picks; mute removes energy/no resurrection; smooth bounded bend and
  pitch movement; reset/block/tick determinism; memory metadata; production Lua
  evaluate/lower/render of demo and unchanged lower strings before shared amp.

What would you change before implementing? In particular examine feedback
stability during changing delay, lifetime bounds, and the bus/control approach.
