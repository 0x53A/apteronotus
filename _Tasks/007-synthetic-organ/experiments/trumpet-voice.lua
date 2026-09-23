-- Experimental, designed 32-partial reed. The real Trompette comparison
-- motivates a stronger upper-middle plateau and an extended tail. These are
-- hand-designed coefficients, not copied audio or a calibrated pipe model.
local function trumpet_trial(hz, feet, level, cents)
  local weights = {1,.72,.25,.55,.53,.39,.42,.40,.48,.18,.24,.30,
    .23,.12,.15,.12,.10,.085,.075,.055,.05,.04,.03,.025,
    .021,.018,.015,.012,.010,.008,.006,.004}
  local total=0
  for _,w in ipairs(weights) do total=total+w end
  local banks={{},{},{}}
  for i,w in ipairs(weights) do
    local group=i<=2 and 1 or (i<=8 and 2 or 3)
    for k=1,3 do banks[k][i]=k==group and w/total or 0 end
  end
  local f=hz*(8/feet)*2^(cents/1200)
  local color=clamp((220/clamp(f,20,20000))^.10,.65,1.2)
  local scale=math.sqrt(feet/8)
  local low=harmonics(f,banks[1])*adsr(secs(.014*scale),ms(1),1,ms(70))
  local body=harmonics(f,banks[2])*adsr(secs(.022*scale),ms(1),1,ms(70))
  local high=harmonics(f,banks[3])*adsr(secs(.009*scale),ms(45),.8,ms(70))/.8
  return (low+body*color+high*color*color)*level
end
