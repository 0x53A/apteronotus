"""Assemble standalone, editable audition scores; run from repository root."""
from pathlib import Path
source=Path(__file__).resolve().parent
out=source.parents[2]/'target/organ-experiments'
out.mkdir(parents=True,exist_ok=True)
helper=(source/'trumpet-voice.lua').read_text()+'\n'
score=(source/'registration-study.eod').read_text()
for i,name in enumerate(['baseline','bright','spatial','driven','trumpet']):
    (out/(name+'.eod')).write_text(helper+score.replace('local variant = 0',f'local variant = {i}'))
(out/'single-pipes.eod').write_text(helper+(source/'single-pipes.eod').read_text())
