"""Measure the local Frankfurt organ recording; no audio is uploaded.

Requires numpy/scipy/matplotlib. Decode the original first:
  ffmpeg -i /home/lukas/Downloads/IMG_9443.mov -map 0:a:0 -c:a pcm_s16le target/frankfurt-organ/reference.wav
Render frankfurt-probe.eod with --seconds 11.3 --tail 0 to
  target/frankfurt-organ/probe-final-comparison.wav
Run from the repository root. Outputs are derived artifacts under target/.
Spectral peaks are overtones AND played pitches, never a reliable score.
Mean channel power avoids cancellation from summing stereo to mono.
"""
import json
from pathlib import Path
import numpy as np
from scipy.io import wavfile
from scipy import signal
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
root=Path('target/frankfurt-organ')
sr,x=wavfile.read(root/'reference.wav'); x=x.astype(float)/32768
f,t,z=signal.stft(x,fs=sr,nperseg=16384,noverlap=14336,axis=0,boundary=None)
p=np.mean(np.abs(z)**2,axis=1)
mask=(f>40)&(f<8000)
fp,power=signal.welch(x,fs=sr,nperseg=16384,axis=0); power=power.mean(axis=1)
centroid=float(np.sum(fp*power)/np.sum(power))
regions=[(0,200),(200,2000),(2000,8000),(8000,sr/2)]
metrics={'seconds':len(x)/sr,'sample_rate':sr,'rms_dbfs':float(20*np.log10(np.sqrt(np.mean(x*x)))),'peak_dbfs':float(20*np.log10(np.max(np.abs(x)))),'centroid_hz':centroid,'power_fraction':{f'{a}-{b}':float(power[(fp>=a)&(fp<b)].sum()/power.sum()) for a,b in regions},'side_mid_db':float(10*np.log10(np.sum((x[:,0]-x[:,1])**2)/np.sum((x[:,0]+x[:,1])**2)))}
names=['C','C#','D','Eb','E','F','F#','G','Ab','A','Bb','B']
def name(hz):
 m=69+12*np.log2(hz/440); k=int(round(m)); return names[k%12]+str(k//12-1),round(float((m-k)*100),1)
rows=[]
for sec in np.arange(.3,10.9,.4):
 j=np.argmin(abs(t-sec)); spec=p[:,max(0,j-1):j+2].mean(axis=1)
 peaks,_=signal.find_peaks(spec,distance=4,prominence=spec.max()*.008)
 peaks=sorted([k for k in peaks if 65<f[k]<1600],key=lambda k:spec[k],reverse=True)[:9]
 row={'seconds':round(float(sec),2),'peaks':[{'hz':round(float(f[k]),2),'note':name(f[k])[0],'cents':name(f[k])[1],'relative_db':round(float(10*np.log10(spec[k]/spec.max())),1)} for k in peaks]}; rows.append(row)
metrics['peak_snapshots']=rows
metrics['identification']={
 'church':'Kaiserdom St. Bartholomäus, Frankfurt am Main',
 'visible_instrument':'Klais main organ, Opus 1109, inaugurated 1957, expanded 1994',
 'evidence':'video frame, embedded location and parish instrument description; sounding divisions cannot be proven from the frame',
 'recorded_local_time':'2025-04-22T13:45:10+02:00',
 'date_source':'com.apple.quicktime.creationdate; container creation_time is a later export',
 'piece':None,
 'piece_status':'Unidentified. No programme found for the recorded date/time; spectral peaks do not establish a title.',
 'sources':[
  'https://www.dom-frankfurt.de/dompfarrei/kirchorte/dom-st-bartholomaeus/dommusik/domorgel',
  'https://www.domkonzerte.de/kontakt/presse'],
}
import hashlib
metrics['reference_sha256']=hashlib.sha256((root/'reference.wav').read_bytes()).hexdigest()
probe=root/'probe-final-comparison.wav'
comparison=None
if probe.exists():
 rate,y=wavfile.read(probe)
 if np.issubdtype(y.dtype,np.integer): y=y.astype(float)/(-np.iinfo(y.dtype).min)
 y=y[:int(11.3*rate)]
 yf,yp=signal.welch(y,fs=rate,nperseg=round(16384*rate/sr),axis=0)
 yp=yp.mean(axis=1)
 comparison={'seconds':len(y)/rate,'centroid_hz':float(np.sum(yf*yp)/yp.sum()),
  'power_above_2khz_fraction':float(yp[yf>=2000].sum()/yp.sum()),
  'side_mid_db':float(10*np.log10(np.sum((y[:,0]-y[:,1])**2)/np.sum((y[:,0]+y[:,1])**2))),
  'peak_dbfs':float(20*np.log10(np.max(np.abs(y)))),
  'sha256':hashlib.sha256(probe.read_bytes()).hexdigest(),
  'limits':'Approximate pitch-set probe, not a transcription or isolated-stop match. Similar aggregate power and width do not prove timbral fidelity. The phone, room and score affect all these measures; no isolated decay establishes RT60.'}
 metrics['synthesis_comparison']=comparison
(root/'reference-analysis.json').write_text(json.dumps(metrics,indent=2)+'\n')
fig,axs=plt.subplots(3,1,figsize=(14,11),layout='constrained')
axs[0].plot(np.arange(len(x))/sr,x[:,0],lw=.4);axs[0].set(xlabel='seconds',ylabel='amplitude',title='Frankfurt organ: original recording')
axs[1].pcolormesh(t,f[mask],10*np.log10(p[mask]+1e-15),vmin=-90,vmax=-35,cmap='magma',shading='auto');axs[1].set(yscale='log',ylim=(65,8000),xlabel='seconds',ylabel='Hz',title='Harmonic motion (372 ms Hann window; not a transcription)')
axs[2].semilogx(fp,10*np.log10(power+1e-16));axs[2].set(xlim=(40,12000),ylim=(-110,-30),xlabel='Hz',ylabel='dBFS/Hz',title='Mean channel power spectrum');axs[2].grid(alpha=.2)
fig.savefig(root/'reference-analysis.png',dpi=140)
if comparison:
 fig2,ax=plt.subplots(figsize=(12,5),layout='constrained')
 ax.semilogx(fp,10*np.log10(power/np.mean(x*x)+1e-16),label='Phone recording')
 ax.semilogx(yf,10*np.log10(yp/np.mean(y*y)+1e-16),label='Synthetic pitch-set probe',alpha=.8)
 ax.set(xlim=(40,10000),ylim=(-85,-10),xlabel='Hz',ylabel='PSD relative to total RMS (dB/Hz)',title='Frankfurt organ: recording and approximate synthesis, independently level matched')
 ax.grid(alpha=.2);ax.legend()
 fig2.savefig(root/'comparison.png',dpi=140)
print(json.dumps({k:v for k,v in metrics.items() if k not in ('peak_snapshots','identification')},indent=2))
for row in rows: print(row['seconds'], ' '.join(f"{q['note']}({q['relative_db']})" for q in row['peaks']))
