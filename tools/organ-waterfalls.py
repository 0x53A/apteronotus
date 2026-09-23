#!/usr/bin/env python3
"""Reproducible stereo-power spectrograms and waterfall plots of local WAVs.

Dependencies: numpy, scipy, matplotlib. No downloading or source separation.
Run from the repository root; see reference-analysis/README.md for the manifest.
"""
import argparse
import hashlib
from html import escape
import json
from pathlib import Path

import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt
from matplotlib.ticker import FuncFormatter
import numpy as np
from scipy import signal
from scipy.io import wavfile

RATE = 24000
FLOOR = -100
TOP = -25
TICKS = [50, 100, 200, 500, 1000, 2000, 4000, 8000]
plt.rcParams.update({'font.size': 10, 'axes.spines.top': False,
                     'axes.spines.right': False, 'savefig.dpi': 160})


def db(power):
    return 10 * np.log10(np.maximum(power, 1e-16))


def stamp(t, _=None):
    return f'{int(t)//60}:{t%60:04.1f}'


def read(path):
    rate, x = wavfile.read(path)
    if np.issubdtype(x.dtype, np.integer):
        if x.dtype == np.uint8:
            x = (x.astype(np.float64) - 128) / 128
        else:
            x = x.astype(np.float64) / (np.iinfo(x.dtype).max + 1)
    else:
        x = x.astype(np.float64)
    if x.ndim == 1:
        x = x[:, None]
    if not np.isfinite(x).all():
        raise ValueError(f'Nonfinite audio: {path}')
    if rate != RATE:
        gcd = np.gcd(rate, RATE)
        x = signal.resample_poly(x, RATE // gcd, rate // gcd, axis=0)
    return x


def spectrum(x, n=4096, hop=256):
    # Average channel POWERS, never sum waveforms: avoids stereo cancellation.
    powers = []
    for channel in x.T:
        f, t, p = signal.spectrogram(channel, RATE, window='hann', nperseg=n,
                                   noverlap=n-hop, detrend=False, scaling='density')
        powers.append(p)
    return f, t, np.mean(powers, axis=0)


def metrics(x, f, p):
    mean = p.mean(axis=1)
    df = f[1]-f[0]
    total = np.sum(mean)*df
    bands = [(20, 200), (200, 1000), (1000, 2000), (2000, 4000), (4000, 8000), (8000, 12000)]
    result = {'rms_dbfs': float(db(np.mean(x*x))), 'peak_dbfs': float(20*np.log10(max(np.max(np.abs(x)), 1e-8))),
              'centroid_hz': float(np.sum(f*mean)/max(np.sum(mean), 1e-16)),
              'bands_power_percent': {f'{a}-{b}': float(100*np.sum(mean[(f>=a)&(f<b)])*df/max(total, 1e-16)) for a,b in bands}}
    if x.shape[1] == 2:
        mid = (x[:, 0]+x[:, 1])/2
        side = (x[:, 0]-x[:, 1])/2
        result['side_mid_db'] = float(db(np.mean(side*side)/max(np.mean(mid*mid), 1e-16)))
    return result


def heatmap(ax, f, t, p, start=0, low=40, high=8000):
    choose = (f>=low)&(f<=high)
    im = ax.pcolormesh(t+start, f[choose], db(p[choose]), cmap='magma',
                       shading='auto', vmin=FLOOR, vmax=TOP, rasterized=True)
    ax.set_yscale('log')
    ticks = [v for v in TICKS if low<=v<=high]
    ax.set_yticks(ticks, [str(v) for v in ticks])
    ax.set_ylim(low, high)
    ax.set_ylabel('Frequency (Hz, log)')
    ax.set_xlabel('Time in source (m:ss)')
    ax.xaxis.set_major_formatter(FuncFormatter(stamp))
    return im


def save(fig, out, name):
    fig.savefig(out / (name+'.png'), bbox_inches='tight')
    fig.savefig(out / (name+'.pdf'), bbox_inches='tight')
    plt.close(fig)


def overview(x, title, out, name, regions):
    gain = 10**(-20/20) / max(np.sqrt(np.mean(x*x)), 1e-12)
    f,t,p = spectrum(x*gain, hop=1024)
    fig, axs = plt.subplots(2, 1, figsize=(15, 7), gridspec_kw={'height_ratios':[1,4]}, sharex=True)
    frame = RATE//10
    y = x[:len(x)//frame*frame].reshape(-1, frame, x.shape[1])
    rms = db(np.mean(y*y, axis=(1,2)))
    axs[0].plot((np.arange(len(rms))+.5)/10, rms, lw=.7, color='#326a9c')
    axs[0].set_ylabel('100 ms RMS\n(original dBFS)')
    axs[0].set_ylim(-65, 0)
    im = heatmap(axs[1], f,t,p)
    for region in regions:
        a,b = region['start'],region['end']
        for ax in axs:
            ax.axvspan(a,b,alpha=.13,color='cyan')
        axs[1].text((a+b)/2, 6800, region['id'], ha='center', color='cyan', fontsize=9)
    fig.subplots_adjust(left=.08, right=.86, bottom=.10, top=.90, hspace=.12)
    fig.colorbar(im, cax=fig.add_axes([.89,.10,.018,.59]), label='PSD (dBFS/Hz); whole track normalized to −20 dBFS RMS')
    fig.suptitle(title+' — whole-track overview')
    save(fig,out,name+'-overview')


def zoom(x, title, start, end, out, name, note):
    clip = x[round(start*RATE):round(end*RATE)]
    if len(clip) < 8192 or start < 0 or end > len(x)/RATE + .001:
        raise ValueError(f'Invalid window: {name} {start}..{end}')
    gain = .1/max(np.sqrt(np.mean(clip*clip)), 1e-12)
    f,t,p = spectrum(clip*gain, n=8192, hop=256)
    f2,t2,p2 = spectrum(clip*gain, n=1024, hop=128)
    fig,axs = plt.subplots(2,1,figsize=(14,8), sharex=True)
    im=heatmap(axs[0],f,t,p,start)
    axs[0].set_title('Harmonic detail: 341 ms Hann window, 2.93 Hz bins (smears attacks)')
    heatmap(axs[1],f2,t2,p2,start,low=100)
    axs[1].set_title('Timing detail: 42.7 ms Hann window, 23.4 Hz bins (blends low harmonics)')
    fig.subplots_adjust(left=.08, right=.86, bottom=.10, top=.87, hspace=.32)
    fig.colorbar(im,cax=fig.add_axes([.89,.10,.018,.77]),label='PSD (dBFS/Hz); excerpt normalized to −20 dBFS RMS')
    fig.suptitle(f'{title} · {stamp(start)}–{stamp(end)}\n{note}',fontsize=12)
    save(fig,out,name+'-zoom')
    # True waterfall: frequency × source time × power density. Slice decimation
    # is for display only. The heatmaps retain all computed time frames.
    fig=plt.figure(figsize=(14,8))
    fig.subplots_adjust(left=0, right=.94, bottom=.08, top=.89)
    ax=fig.add_subplot(111,projection='3d')
    use=(f>=40)&(f<=8000)
    xx=np.log10(f[use])
    indices=np.unique(np.linspace(0,len(t)-1,min(65,len(t))).astype(int))
    for j,i in enumerate(indices):
        zz=np.clip(db(p[use,i]),FLOOR,TOP)
        ax.plot(xx,np.full(len(xx),t[i]+start),zz,lw=.65,color=plt.cm.viridis(j/max(len(indices)-1,1)),alpha=.9)
    ax.set_xticks(np.log10(TICKS),[str(v) for v in TICKS])
    ax.set_xlim(np.log10(40),np.log10(8000))
    ax.set_zlim(FLOOR,TOP)
    ax.set_xlabel('Frequency (Hz, log)',labelpad=12)
    ax.set_ylabel('Source time (s)',labelpad=12)
    ax.set_zlabel('PSD (dBFS/Hz)',labelpad=12)
    ax.view_init(elev=32,azim=-65)
    ax.set_title(f'{title} · {stamp(start)}–{stamp(end)}\nWaterfall · 341 ms Hann · excerpt RMS −20 dBFS',pad=20)
    save(fig,out,name+'-waterfall')
    rawf,rawt,rawp=spectrum(clip,n=8192,hop=256)
    result=metrics(clip,rawf,rawp)
    result.update({'start_seconds':start,'end_seconds':end,'normalization_gain_db':float(20*np.log10(gain)), 'note':note})
    # Store normalized PSD for consistent overlays without reinterpreting levels.
    return result,(f,p.mean(axis=1))


def gallery(manifest, results, out):
    parts = ["""<!doctype html><html lang="en"><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>Darktide organ — spectral comparison</title>
<style>body{font:17px/1.55 system-ui,sans-serif;margin:2rem auto;padding:0 1.2rem;max-width:1200px;color:#20232a;background:#f7f8fa}h1,h2,h3{line-height:1.2}img{width:100%;height:auto;background:white}figure{margin:1.5rem 0}a{color:#165999}table{border-collapse:collapse;width:100%;font-size:15px}th,td{padding:.5rem;text-align:left;border-bottom:1px solid #ccd}summary{cursor:pointer;padding:1rem 0;font-weight:bold}.note{background:#e5edf5;padding:1rem}nav a{margin-right:1rem}</style>
<h1>Darktide organ: waterfalls and comparison</h1>
<p>Measured from the two linked YouTube recordings and the production Apteronotus renderer.
These are time–frequency waterfalls, not impulse-response decay measurements.</p>
<p class="note">Instrument identification is provisional: sustained harmonic ladders can be organ,
processed synth, strings or combined sources. No stems were isolated and no listening judgment is claimed.
All excerpt plots use the same −20 dBFS mean-channel RMS normalization and PSD scale.
Frequency bins are 2.93 Hz in waterfalls; the 341 ms window blurs rapid attacks.</p>
<nav><a href="#light">Light of the Imperium</a><a href="#empires">Empires Will Fall</a><a href="#comparisons">Comparisons</a><a href="#iron">Our organ</a><a href="measurements.json">Measurements JSON</a></nav>"""]
    for paragraph in manifest.get('findings', []):
        parts.append('<p>'+escape(paragraph)+'</p>')
    def picture(name, caption):
        parts.append(f'<figure><a href="{name}.png"><img loading="lazy" src="{name}.png" alt="{escape(caption)}"></a><figcaption>{escape(caption)} · <a href="{name}.pdf">PDF</a></figcaption></figure>')
    parts.append('<h2 id="comparisons">Level-matched spectral comparisons</h2>')
    for comparison in manifest.get('comparisons', []):
        picture(comparison['id'], comparison['title'])
    parts.append('<h2>Measurements</h2><p>Original decoded/resampled levels; spectral centroid and upper-band fraction describe the entire excerpt, including other instruments. Mono side/mid is shown as −∞.</p><table><tr><th>Passage</th><th>RMS dBFS</th><th>Centroid Hz</th><th>2–12 kHz power %</th><th>Side/mid dB</th></tr>')
    for source in results['sources']:
        for rid, m in source['measurements'].items():
            high = sum(m['bands_power_percent'][b] for b in ['2000-4000','4000-8000','8000-12000'])
            side = m.get('side_mid_db', -160)
            side_text = '−∞' if side < -150 else f'{side:.1f}'
            parts.append(f"<tr><td>{escape(source['id']+' / '+rid)}</td><td>{m['rms_dbfs']:.1f}</td><td>{m['centroid_hz']:.0f}</td><td>{high:.3f}</td><td>{side_text}</td></tr>")
    parts.append('</table>')
    for source in manifest['sources']:
        sid=source['id']
        parts.append(f'<h2 id="{sid}">{escape(source["title"])}</h2>')
        if 'url' in source:
            parts.append(f'<p><a href="{escape(source["url"])}">Original recording</a></p>')
        picture(sid+'-overview','Whole-track overview; highlighted windows correspond to the zooms below')
        for r in source.get('regions', []):
            name=sid+'-'+r['id']
            parts.append(f'<details><summary>{escape(r["id"])} · {stamp(r["start"])}–{stamp(r["end"])}: {escape(r["note"])}</summary>')
            if 'url' in source:
                parts.append(f'<p><a href="{escape(source["url"])}&amp;t={int(r["start"])}s">Play this passage on YouTube</a></p>')
            picture(name+'-zoom','Harmonic detail above; transient detail below. Different window sizes give different PSD peak heights.')
            picture(name+'-waterfall','Frequency × time × power density, up to 65 time slices; use the spectrogram for obscured ridges')
            parts.append('</details>')
    parts.append('<p>Decoded references are lossy YouTube Opus, not studio stems. Channel powers are averaged before plotting; no mono phase cancellation. All audio is resampled to 24 kHz. Overviews use 171 ms Hann windows; zooms use 341 ms and 42.7 ms. Comparison curves integrate linear PSD into 1/6-octave bands. SHA-256 input hashes and exact windows are in measurements.json.</p></html>')
    (out/'index.html').write_text('\n'.join(parts))


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('manifest',type=Path)
    parser.add_argument('--output',type=Path,required=True)
    parser.add_argument('--overview-only',action='store_true')
    args=parser.parse_args()
    manifest=json.loads(args.manifest.read_text())
    args.output.mkdir(parents=True,exist_ok=True)
    results={'sample_rate':RATE,'normalization':'per-track overview; per-excerpt zoom, −20 dBFS stereo mean-power RMS',
             'psd':'one-sided Hann PSD, mean channel powers; no source separation','sources':[]}
    curves={}
    for source in manifest['sources']:
        path=Path(source['path']); x=read(path)
        print(source['id'],len(x)/RATE,flush=True)
        overview(x,source['title'],args.output,source['id'],source.get('regions',[]))
        result={**source,'duration_seconds':len(x)/RATE,'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'measurements':{}}
        if not args.overview_only:
            for region in source.get('regions',[]):
                name=source['id']+'-'+region['id']
                m,curve=zoom(x,source['title'],region['start'],region['end'],args.output,name,region['note'])
                result['measurements'][region['id']]=m
                curves[name]=(source['title']+' / '+region['id'],curve)
        results['sources'].append(result)
    if not args.overview_only:
        for comparison in manifest.get('comparisons',[]):
            fig,ax=plt.subplots(figsize=(13,6))
            fig.subplots_adjust(left=.09,right=.97,bottom=.12,top=.88)
            for name in comparison['regions']:
                title,(f,p)=curves[name]
                # Integrate in disjoint 1/6-octave bins. No interpolation or
                # averaging of dB values: each band reports power, not PSD.
                edges=40*2**(np.arange(0,48)/6)
                vals=np.array([np.sum(p[(f>=a)&(f<b)])*(f[1]-f[0]) for a,b in zip(edges[:-1],edges[1:])])
                ax.plot(np.sqrt(edges[:-1]*edges[1:]),db(vals),label=title,lw=1.4)
            ax.set_xscale('log'); ax.set_xlim(40,8000); ax.set_ylim(-85,-15)
            ax.set_xticks(TICKS,[str(v) for v in TICKS]); ax.grid(alpha=.2)
            ax.set_xlabel('Frequency (Hz, log)'); ax.set_ylabel('1/6-octave band power (dBFS)')
            ax.set_title(comparison['title']+'\nEach excerpt matched to −20 dBFS RMS; different notes/mixes are not a stop-fidelity test')
            ax.legend(fontsize=9)
            save(fig,args.output,comparison['id'])
    (args.output/'measurements.json').write_text(json.dumps(results,indent=2)+'\n')
    if not args.overview_only:
        gallery(manifest, results, args.output)

if __name__=='__main__':
    main()
