"""Prepare the embedded, common-EQ MIT KEMAR data; not used at runtime.
Requires the original SOFA in data/alders-hrtf-2026-10-06/kemar.sofa.
"""
from pathlib import Path
import h5py,numpy as np,struct,hashlib,json
from scipy import signal,ndimage
source=Path('data/alders-hrtf-2026-10-06/kemar.sofa')
with h5py.File(source) as f:
 positions=f['SourcePosition'][:];raw=f['Data.IR'][:];sr=int(f['Data.SamplingRate'][0]);assert sr==44100 and np.all(f['Data.Delay'][:]==0)
response=ndimage.gaussian_filter1d(np.sqrt(np.maximum(np.mean(abs(np.fft.rfft(raw,2048))**2,axis=(0,1)),1e-12)),6)
inverse=np.clip(np.median(response[10:300])/response,.355,2.818)
eq=signal.minimum_phase(signal.firwin2(257,np.linspace(0,1,len(inverse)),inverse))
h=np.array([[signal.fftconvolve(v,eq) for v in pair] for pair in raw])
angles=np.deg2rad(positions[:,:2]);dirs=np.stack([np.cos(angles[:,1])*np.cos(angles[:,0]),np.cos(angles[:,1])*np.sin(angles[:,0]),np.sin(angles[:,1])],axis=1)
front=np.argmax(dirs@np.array([1.,0.,0.]));h/=np.sqrt(np.mean(np.sum(h[front]**2,axis=1)))
out=Path('crates/synth/assets/hrtf/kemar.bin');out.write_bytes(b'APHR'+struct.pack('<III',len(h),h.shape[2],sr)+dirs.astype('<f4').tobytes()+h.astype('<f4').tobytes())
meta={'source':'https://sofacoustics.org/data/database/mit/mit_kemar_normal_pinna.sofa','source_sha256':hashlib.sha256(source.read_bytes()).hexdigest(),'prepared_sha256':hashlib.sha256(out.read_bytes()).hexdigest(),'measurements':len(h),'samples_per_ear':h.shape[2],'sample_rate':sr,'preparation':'Common bounded diffuse-field minimum-phase EQ; one global frontal-energy normalization. No per-ear normalization.','credit':'Gardner, W. G., and Martin, K. D. (1995). HRTF measurements of a KEMAR. JASA 97, 3907–3908.'}
out.with_suffix('.json').write_text(json.dumps(meta,indent=2)+'\n');print(meta)
