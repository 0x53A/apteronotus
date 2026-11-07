-- Concert-flute voicing from harmonic groups and breath, not sampled audio.
-- Register balance was informed by Iowa non-vibrato mf C5–B5 recordings.
-- This is a designed instrument, distinct from the stopped organ-flute rank.
do
  local fields={attack=true,release=true,vibrato=true,rate=true,breath=true,pressure=true,brightness=true}
  local function number(x,name,lo,hi)
    if type(x)~='number' or x~=x or x<lo or x>hi then
      error('flute '..name..' must be finite in '..lo..'..'..hi)
    end
    return x
  end
  function flute(hz,spec)
    spec=spec or {}
    if type(spec)~='table' then error('flute expects an options table') end
    for k in pairs(spec) do if not fields[k] then error('unknown flute field: '..tostring(k)) end end
    local depth=number(spec.vibrato or 8,'vibrato cents',0,40)
    local rate=number(spec.rate or 5.2,'vibrato rate',1,9)
    local air=number(spec.breath or .022,'breath',0,.15)
    local bright=number(spec.brightness or .7,'brightness',0,1)
    local pressure=clamp(spec.pressure or .8,0,1)
    local movement=sine(rate*init_random(181,.97,1.03))*ramp(ms(450),ms(220))
    local frequency=hz*2^((movement*depth-decay(ms(55))*4)/1200)
    local high=clamp((hz-600)/300,0,1)
    local fundamental=harmonics(frequency,{1})
    local low_modes=harmonics(frequency,{0,.48,.25,.09,.04,.012,.01,.009})
    local high_modes=harmonics(frequency,{0,.10,.055,.022,.013,.007,.004,.003})
    local upper=(low_modes*(1-high)+high_modes*high)*( .35+.65*pressure )*(.6+.4*bright)
    local tone=fundamental+upper*(1+decay(ms(45))*.16)
    local breath=noise() >> highpass(900) >> lowpass(6800)
    local tongue=(noise() >> bandpass(1800,.7))*decay(ms(24))*air*.8
    local gate=adsr(spec.attack or ms(45),ms(90),.94,spec.release or ms(90))
    return (tone*(1+movement*.014)+breath*air+tongue)*pressure*gate*.65
  end
end
