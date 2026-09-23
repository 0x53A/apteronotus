-- Synthetic pipe voicing, staged entirely at edit time. These are designed
-- spectra, not sampled stops or a fluid-dynamics model. The room is external.
do
  local ranks = {
    principal = { spectrum = {1, .48, .30, .18, .11, .075, .048, .032, .020, .013, .008, .005},
      attack = .025, release = .085, chiff = .018 },
    flute = { spectrum = {1, .025, .19, .012, .055, .006, .018, .003, .006},
      attack = .045, release = .110, chiff = .025 },
    string = { spectrum = {.65, .58, .48, .36, .27, .20, .145, .105, .075, .054, .038, .027, .019, .013, .009, .006},
      attack = .065, release = .100, chiff = .009 },
    reed = { spectrum = {1, .72, .62, .48, .38, .29, .23, .18, .14, .11, .085, .066, .050, .038, .029, .022},
      attack = .018, release = .060, chiff = .008 },
  }
  local fields = { kind=true, feet=true, cents=true, level=true, attack=true, release=true, chiff=true, voicing=true }
  local function number(value, name, low, high)
    if type(value) ~= "number" or value ~= value or value < low or value > high then
      error(name .. " must be a finite number in " .. low .. ".." .. high)
    end
    return value
  end

  -- Mono, keyed on the containing voice's note clock. hz, and level, may be
  -- graph signals. Other voicing choices stage topology/data, never callbacks.
  function organ_pipe(hz, spec)
    spec = spec or {}
    if type(spec) ~= "table" then error("organ_pipe expects a stop table") end
    for key in pairs(spec) do
      if not fields[key] then error("unknown organ stop field: " .. tostring(key)) end
    end
    local rank = ranks[spec.kind or "principal"]
    if not rank then error("organ kind must be principal, flute, string or reed") end
    local feet = number(spec.feet or 8, "organ feet", .5, 32)
    local cents = number(spec.cents or 0, "organ cents", -100, 100)
    local chiff = number(spec.chiff or rank.chiff, "organ chiff", 0, .2)
    local weights, total = {}, 0
    for _, weight in ipairs(rank.spectrum) do total = total + weight end
    for i, weight in ipairs(rank.spectrum) do weights[i] = weight / total end
    local frequency = hz * (8 / feet) * (2 ^ (cents / 1200))
    -- Bigger pipes speak and close more slowly; this is a deliberate voicing
    -- rule, not an acoustic measurement. No event-random detuning on retrigger.
    local scale = math.sqrt(feet / 8)
    local attack = spec.attack or secs(rank.attack * scale)
    local release = spec.release or secs(rank.release * scale)
    local voicing = spec.voicing or "speech"
    if voicing ~= "speech" and voicing ~= "classic" then
      error("organ voicing must be speech or classic")
    end
    local body
    if voicing == "classic" then
      body = harmonics(frequency, weights) * adsr(attack, ms(1), 1, release)
    else
      -- Three phase-coherent harmonic groups: foundation, body, upper speech.
      -- A pitched stop is voiced across the compass, not merely transposed.
      -- This is deliberately gentle: treble pipes lose upper weight while the
      -- bass keeps more. Values at A3 retain the original steady spectrum.
      local color = clamp((220 / clamp(frequency, 20, 20000)) ^ .20, .55, 1.30)
      local banks = {{}, {}, {}}
      for i, weight in ipairs(weights) do
        local group = 1
        if i >= 7 then group = 3 elseif i >= 3 then group = 2 end
        for k = 1, 3 do banks[k][i] = k == group and weight or 0 end
      end
      local foundation = harmonics(frequency, banks[1])
        * adsr(spec.attack or secs(rank.attack * scale * .75), ms(1), 1, release)
      local middle = harmonics(frequency, banks[2])
        * adsr(spec.attack or secs(rank.attack * scale * 1.35), ms(1), 1, release)
      -- Upper modes briefly overshoot their settled level. At key release,
      -- each envelope closes from its actual current level, including in attack.
      local upper = harmonics(frequency, banks[3])
        * adsr(spec.attack or secs(rank.attack * scale * .50), ms(65), .72, release) / .72
      body = foundation + middle * color + upper * color * color
    end
    if chiff > 0 then
      -- A short upper-mode transient plus air. Both remain inside the final
      -- keyed envelope, including when a very short key is released in attack.
      local air = noise() >> bandpass(clamp(frequency * 5, 350, 5000), .7)
      local speech = harmonics(frequency, {0, 1}) * .65 + air * .35
      body = body + speech * decay(ms(28)) * chiff * adsr(attack, ms(1), 1, release)
    end
    return body * (spec.level or 1)
  end

  -- A registration is a sum of independently voiced ranks. Adding a stop
  -- never renormalizes or quietly turns down the stops already drawn.
  -- No velocity response: the score may explicitly multiply by n.velocity.
  function organ(hz, stops)
    stops = stops or {{kind="principal", feet=8}}
    if type(stops) ~= "table" or #stops < 1 or #stops > 16 then
      error("organ expects 1..16 stop tables")
    end
    local sum = dc(0)
    for i = 1, #stops do sum = sum + organ_pipe(hz, stops[i]) end
    return sum
  end
end
