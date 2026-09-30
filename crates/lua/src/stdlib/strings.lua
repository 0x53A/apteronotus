-- Designed bowed strings, composed at edit time from existing graph nodes.
-- Harmonic stick/slip colour, broad body modes and rosin noise: no recordings
-- or physical bow solver. The containing voice supplies velocity, pan and room.
do
  local bodies = {
    violin = {low=460, middle=1050, bridge=2700, attack=.085, release=.13},
    viola = {low=350, middle=850, bridge=2200, attack=.11, release=.17},
    cello = {low=190, middle=560, bridge=1700, attack=.15, release=.22},
  }
  local fields = {kind=true, attack=true, release=true, vibrato=true,
    rate=true, bow=true, brightness=true, pressure=true}
  local function number(value, name, low, high)
    if type(value) ~= "number" or value ~= value or value < low or value > high then
      error("bowed_string " .. name .. " must be finite in " .. low .. ".." .. high)
    end
    return value
  end

  -- Mono; note-clock envelopes make this a voice instrument, not a patch.
  -- hz and pressure accept graph signals. Other fields are staged choices.
  function bowed_string(hz, spec)
    spec = spec or {}
    if type(spec) ~= "table" then error("bowed_string expects an options table") end
    for key in pairs(spec) do
      if not fields[key] then error("unknown bowed_string field: " .. tostring(key)) end
    end
    local body = bodies[spec.kind or "violin"]
    if not body then error("bowed_string kind must be violin, viola or cello") end
    local depth = number(spec.vibrato or 11, "vibrato cents", 0, 40)
    local rate = number(spec.rate or 5.6, "vibrato rate", 1, 9)
    local bow = number(spec.bow or .018, "bow noise", 0, .1)
    local bright = number(spec.brightness or .65, "brightness", 0, 1)
    local pressure = clamp(spec.pressure or 1, 0, 1)
    -- Vibrato grows after the bow has found the pitch; separate onsets have
    -- reproducible small intonation/rate differences, never shared RNG state.
    local settling = decay(ms(65)) * -9
    local movement = sine(rate * init_random(71, .96, 1.04))
      * ramp(ms(380), ms(180)) * depth
    local frequency = hz * 2 ^ ((settling + movement + init_random(72, -1.8, 1.8)) / 1200)
    local weights, total = {}, 0
    for i = 1, 24 do
      local weight = (1 / i) * math.exp(-i * (.025 + (1 - bright) * .075))
      if i == 1 then weight = weight * .62 end
      weights[i] = weight
      total = total + weight
    end
    for i = 1, 24 do weights[i] = weights[i] / total end
    local string = harmonics(frequency, weights)
    -- Broad, fixed-Hz body colour stays in place as pitch changes. Each
    -- branch receives the same source, rather than a second detuned oscillator.
    local wood = string * .72
      + (string >> bandpass(body.low, 1.5)) * .58
      + (string >> bandpass(body.middle, 1.1)) * .34
      + (string >> bandpass(body.bridge, .8)) * (.3 + bright * .5) * pressure
    local rosin = noise() >> highpass(900) >> lowpass(6500)
    local speech = 1 + decay(ms(90)) * .22
    local tone = wood * (1 - .08 * pressure + sine(3.7) * .008)
      + rosin * bow * pressure * speech
    return tone * adsr(spec.attack or secs(body.attack), ms(160), .92,
      spec.release or secs(body.release)) * .95
  end
end
