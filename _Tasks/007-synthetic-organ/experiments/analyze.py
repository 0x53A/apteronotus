#!/usr/bin/env python3
"""Run from repository root after rendering the scores and extracting references."""
import importlib.util
import json
import struct
import hashlib
from pathlib import Path
from html import escape
import numpy as np
import matplotlib.pyplot as plt
from scipy.io import wavfile

ROOT = Path(__file__).resolve().parents[3]
spec = importlib.util.spec_from_file_location('waterfalls', ROOT/'tools/organ-waterfalls.py')
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
OUT = ROOT/'target/organ-experiments'
PLOTS = OUT/'plots'
PLOTS.mkdir(exist_ok=True)
NAMES = ['baseline','bright','spatial','driven','trumpet']
DESCRIPTIONS = ['Original seven-rank registration and quiet room',
                'More reed, octave and mixture weight; center placement',
                'Bright registration, ranks spread across stereo, stronger room',
                'Spatial version with gentle per-rank tanh saturation',
                'Spatial version with a designed 32-partial trumpet-like reed; no drive']


def loop_bounds(path):
    """Read the first RIFF smpl sustain loop; end sample is inclusive."""
    b=path.read_bytes(); pos=12; rate=None
    while pos+8<=len(b):
        tag=b[pos:pos+4]; n=struct.unpack_from('<I',b,pos+4)[0]
        d=b[pos+8:pos+8+n]
        if tag==b'fmt ': rate=struct.unpack_from('<I',d,4)[0]
        if tag==b'smpl' and struct.unpack_from('<I',d,28)[0]:
            loop=struct.unpack_from('<6I',d,36)
            return loop[2]/rate,(loop[3]+1)/rate
        pos+=8+n+(n%2)
    raise ValueError(f'No sustain loop: {path}')


def measure(x):
    n=min(8192,len(x))
    f,t,p=m.spectrum(x,n=n,hop=min(256,n))
    result=m.metrics(x,f,p)
    result['upper_percent']=sum(result['bands_power_percent'][b] for b in ['2000-4000','4000-8000','8000-12000'])
    return result


def band_curve(ax,x,label):
    x=x*.1/np.sqrt(np.mean(x*x))
    f,t,p=m.spectrum(x,n=min(8192,len(x)),hop=256)
    power=p.mean(axis=1);edges=40*2**(np.arange(48)/6)
    vals=[power[(f>=a)&(f<b)].sum()*(f[1]-f[0]) for a,b in zip(edges[:-1],edges[1:])]
    ax.plot(np.sqrt(edges[:-1]*edges[1:]),m.db(vals),label=label)


def harmonic_curve(x):
    # Native loop duration determines resolution; no fake long sustain made by
    # repeating a short loop. Integrate each harmonic's Hann main lobe.
    n=min(8192,len(x));f,t,p=m.spectrum(x,n=n,hop=min(256,n));power=p.mean(axis=1)
    expected=220
    possible=np.flatnonzero((f>expected*.93)&(f<expected*1.07))
    k=possible[np.argmax(power[possible])]
    y=np.log(np.maximum(power[k-1:k+2],1e-30))
    offset=.5*(y[0]-y[2])/(y[0]-2*y[1]+y[2])
    hz=(k+offset)*(f[1]-f[0])
    half=min(70,2.5*(f[1]-f[0]))
    hp=np.array([power[abs(f-hz*h)<=half].sum()*(f[1]-f[0]) for h in range(1,25)])
    return hz,m.db(hp/max(hp[0],1e-16))


def main():
    report={'variants':{},'pipes':{},'method':'Stereo mean-power PSD at 24 kHz; variants 1–3 s; real pipes use first RIFF sustain loop. Harmonic plots use loop-dependent Hann resolution; ±2.5 bins (max 70 Hz) integrated around estimated A3 harmonics, relative to fundamental. No noise-floor subtraction.'}
    audio=[]
    fig,ax=plt.subplots(figsize=(12,6),layout='constrained')
    for name in NAMES:
        path=OUT/(name+'.wav'); x=m.read(path)
        report['variants'][name]=measure(x[m.RATE:3*m.RATE])
        report['variants'][name]['sha256']=hashlib.sha256(path.read_bytes()).hexdigest()
        rate,original=wavfile.read(path)
        assert np.isfinite(original).all() and np.max(abs(original))<1
        gain=.1/np.sqrt(np.mean(original[:12*rate].astype(float)**2))
        normalized=original.astype(float)*gain
        assert np.max(abs(normalized))<.98, 'Normalize with more headroom'
        wavfile.write(OUT/(name+'-matched.wav'),rate,np.round(normalized*32767).astype(np.int16))
        audio.append(normalized)
        audio.append(np.zeros((int(.75*rate),2)))
        report['variants'][name]['audition_gain_db']=float(20*np.log10(gain))
        report['variants'][name]['audition_peak_dbfs']=float(20*np.log10(np.max(abs(normalized))))
        band_curve(ax,x[m.RATE:3*m.RATE],name)
        m.zoom(x,name+' organ',0,4,PLOTS,name,'Same chord and key duration; excerpt RMS matched')
    # Reference is a mixed soundtrack excerpt, never an isolated-organ target.
    reference=ROOT/'target/organ-reference-analysis/yzQ7IBpQd0Q.wav'
    if reference.exists():
        x=m.read(reference); band_curve(ax,x[round(107.9*m.RATE):round(108.7*m.RATE)],'Empires reference (full mix)')
    ax.set_xscale('log');ax.set_xlim(40,8000);ax.set_ylim(-85,-15)
    ax.set_xticks(m.TICKS,[str(x) for x in m.TICKS]);ax.grid(alpha=.2)
    ax.set_xlabel('Frequency (Hz)');ax.set_ylabel('1/6-octave power, dBFS')
    ax.set_title('Registration trials — same chord, excerpts normalized to −20 dBFS RMS')
    ax.legend();m.save(fig,PLOTS,'variants-spectrum')
    wavfile.write(OUT/'all-variants-matched.wav',rate,np.round(np.concatenate(audio[:-1])*32767).astype(np.int16))

    single=m.read(OUT/'single-pipes.wav')
    refs=OUT/'references'
    samples=[('Montre 8','principal',refs/'Jeuxdorgues2_GrandOrgue/GOMontre8/057-A.wav'),
             ('Bourdon 8','flute',refs/'Jeuxdorgues2_GrandOrgue/GOBourdon8/057-A.wav'),
             ('Salicional 8 (Romanswiller)','string',refs/'Jeuxdorgues2_GrandOrgue/GOSalicional8/057-A.wav'),
             ('Trompette 8','reed',refs/'Jeuxdorgues2_GrandOrgue/GOTrompette8/057-A.wav'),
             ('Gedackt 8 (Bureå)','flute',refs/'Burea_Funeral_Chapel/Gedackt8/057-A.wav'),
             ('Salicional 8 (Bureå)','string',refs/'Burea_Funeral_Chapel/Salicional8/057-A.wav')]
    fig,axs=plt.subplots(2,2,figsize=(13,9),layout='constrained')
    kinds=['principal','flute','string','reed']
    for i,kind in enumerate(kinds):
        ax=axs.flat[i]; ours=single[(i*4+1)*m.RATE:(i*4+2)*m.RATE]
        hz,curve=harmonic_curve(ours);ax.plot(np.arange(1,25),curve,label='Our '+kind,lw=2)
        report['pipes']['our '+kind]={**measure(ours),'estimated_hz':float(hz),'harmonic_db':curve.tolist()}
        ax.set_title(kind+' · nominal A3');ax.set_xlabel('Harmonic number');ax.set_ylabel('Harmonic power relative to fundamental (dB)')
        ax.set_xlim(1,24);ax.set_ylim(-85,15);ax.set_xticks([1,4,8,12,16,20,24]);ax.grid(alpha=.2)
    trial=single[17*m.RATE:18*m.RATE]
    hz,curve=harmonic_curve(trial)
    axs.flat[3].plot(np.arange(1,25),curve,label='32-partial trumpet trial',lw=2)
    report['pipes']['trumpet trial']={**measure(trial),'estimated_hz':float(hz),'harmonic_db':curve.tolist()}
    for name,kind,path in samples:
        x=m.read(path);a,b=loop_bounds(path);loop=x[round(a*m.RATE):round(b*m.RATE)]
        hz,curve=harmonic_curve(loop)
        report['pipes'][name]={**measure(loop),'path':str(path.relative_to(ROOT)),'loop_seconds':[a,b],'estimated_hz':float(hz),'harmonic_db':curve.tolist(),'sha256':hashlib.sha256(path.read_bytes()).hexdigest()}
        ax=axs.flat[kinds.index(kind)];ax.plot(np.arange(1,25),curve,label=name)
        # Full original recording including release, normalized with SUSTAIN RMS.
        rate,orig=wavfile.read(path)
        assert orig.dtype==np.int16, 'Reference decoder expects verified PCM16 sources'
        orig=orig.astype(float)/32768
        g=.1/np.sqrt(np.mean(orig[round(a*rate):round(b*rate)]**2))
        g=min(g,.95/max(np.max(abs(orig)),1e-9))
        dest='reference-'+path.parent.parent.name+'-'+path.parent.name+'.wav'
        wavfile.write(OUT/dest,rate,np.round(orig*g*32767).astype(np.int16))
        report['pipes'][name]['audition_file']=dest
    for ax in axs.flat:ax.legend(fontsize=8)
    fig.suptitle('Real individual pipes versus our designed ranks — sustain only\nInstrument voicings differ; recording noise dominates very weak upper components')
    m.save(fig,PLOTS,'real-pipe-harmonics')
    performance=OUT/'burea-allegro.wav'
    if performance.exists():
        x=m.read(performance)
        m.overview(x,'Bureå organ-only Allegro — sample-based performance + convolution room',PLOTS,'organ-performance',[dict(id='excerpt',start=10,end=18)])
        m.zoom(x,'Bureå organ-only Allegro',10,18,PLOTS,'organ-performance','Gedackt, Rörflöjt and Principal; different music from our trials')
        report['organ_performance']={**measure(x[10*m.RATE:18*m.RATE]),'url':'https://familjenpalo.se/vpo/examples/','window_seconds':[10,18],'duration_seconds':len(x)/m.RATE,'sha256':hashlib.sha256(performance.read_bytes()).hexdigest()}
    (OUT/'measurements.json').write_text(json.dumps(report,indent=2)+'\n')
    html=['<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Organ trials</title><style>body{font:17px/1.5 system-ui;max-width:1100px;margin:2rem auto;padding:1rem}img{width:100%}audio{width:100%}th,td{padding:.5rem;text-align:left}table{border-collapse:collapse}td{border-top:1px solid #ddd}</style><h1>Organ trials</h1><p>Same original phrase: held chord 0–4 s, chord changes 4–8 s, rhythmic keys 8–12 s, then four seconds of room tail. Files are RMS-matched over the first 12 seconds to −20 dBFS; perceived loudness can still differ.</p><p><a href="all-variants-matched.wav">Download sequential A/B reel</a> (baseline → bright → spatial → driven → trumpet, each 16 s plus 0.75 s between versions).</p>']
    for name,description in zip(NAMES,DESCRIPTIONS):
        html.append(f'<h2>{name.title()}</h2><p>{escape(description)}</p><audio controls preload="none" src="{name}-matched.wav"></audio><p><a href="{name}.eod">Editable score</a> · <a href="plots/{name}-waterfall.png">Waterfall</a> · <a href="plots/{name}-zoom.png">Zoom</a></p>')
    html.append('<h2>Measured held chord, 1–3 s</h2><table><tr><th>Version</th><th>Centroid</th><th>2–12 kHz power</th><th>Side/mid</th></tr>')
    for name,v in report['variants'].items():
        html.append(f'<tr><td>{name}</td><td>{v["centroid_hz"]:.0f} Hz</td><td>{v["upper_percent"]:.2f}%</td><td>{v["side_mid_db"]:.1f} dB</td></tr>')
    html.append('</table><img src="plots/variants-spectrum.png" alt="Registration spectra"><h2>Real organ-only pipe recordings</h2><p>Six original pipe recordings, gain-adjusted for audition. Sample release tails and room remain; no looping or repitching. The harmonic chart uses the declared sustain loops, with different native loop lengths/resolution. Audio is reference material only; the synth remains sample-free.</p><p>Romanswiller: <a href="https://www.jeuxdorgues.com/jeux-d-orgues-2-stiehr-mockers/">Jeux d’orgues 2, Joseph Basquin; GrandOrgue remaster by Graham Goode, Joseph Basquin and Martin (M.XY)</a>, including noise reduction. Bureå: <a href="https://familjenpalo.se/vpo/burea-funeral-chapel/">Lars Palo, Bureå Funeral Chapel</a>, <a href="https://creativecommons.org/licenses/by-sa/2.5/">CC BY-SA 2.5</a>. Local reference copies retain their original rights; gain-adjusted Bureå excerpts are under the same license.</p><img src="plots/real-pipe-harmonics.png" alt="Harmonic comparison of real and synthetic stops">')
    for name,v in report['pipes'].items():
        if 'audition_file' in v:html.append(f'<p>{escape(name)} — estimated {v["estimated_hz"]:.1f} Hz, sustain {v["loop_seconds"][0]:.3f}–{v["loop_seconds"][1]:.3f} s</p><audio controls preload="none" src="{v["audition_file"]}"></audio>')
    if performance.exists():
        html.append('<h2>Organ-only performance</h2><p>Lars Palo plays part of C. P. E. Bach’s Allegro, using Bureå Gedackt, Rörflöjt and Principal in GrandOrgue with JConvolver reverb. This is a sample-based organ performance, not an unprocessed live recording. Different music and registration: use it for context, not a matched error score. <a href="https://familjenpalo.se/vpo/examples/">Source and recording details</a>.</p><audio controls preload="none" src="burea-allegro.mp3"></audio><img src="plots/organ-performance-overview.png" alt="Organ-only performance spectrogram"><p><a href="plots/organ-performance-waterfall.png">10–18 s waterfall</a> · <a href="plots/organ-performance-zoom.png">Zoom</a></p>')
    html.append('<p>These are candidate designs, not an auditory quality verdict. The driven variant uses a non-oversampled shaper; post-filtering is not a proof against aliasing. No synth defaults changed.</p><p><a href="measurements.json">Full measurements</a></p></html>')
    (OUT/'index.html').write_text('\n'.join(html))
    print(json.dumps(report['variants'],indent=2))
    for name,v in report['pipes'].items():print(name,round(v['centroid_hz']),round(v['upper_percent'],3),round(v['estimated_hz'],2))

if __name__=='__main__':main()
