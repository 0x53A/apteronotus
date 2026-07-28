#!/usr/bin/env python3
"""
Strudel corpus audit — what real live-coding idioms would our IR have to hold?

Fetches the corpora, extracts every pattern snippet, and classifies the
constructs. Run it again whenever a better corpus turns up; the numbers in
../LOG.md came from this script on 2026-07-28.

    python3 audit.py [workdir]        # default: ./_work

The sampling bias is the important caveat and the script prints it: the
song corpus is overwhelmingly one author, the docs corpus is pedagogical
one-liners. Findings that appear in *both* columns are the robust ones.
"""

import base64
import json
import pathlib
import re
import subprocess
import sys
import urllib.parse
from collections import Counter, defaultdict

SOURCES = [
    ('strudel', 'https://codeberg.org/uzu/strudel.git'),
    ('songs', 'https://github.com/eefano/strudel-songs-collection.git'),
    ('awesome', 'https://github.com/terryds/awesome-strudel.git'),
]


def clone(work: pathlib.Path) -> None:
    work.mkdir(parents=True, exist_ok=True)
    for name, url in SOURCES:
        if (work / name).exists():
            continue
        print(f'cloning {name} …', file=sys.stderr)
        subprocess.run(['git', 'clone', '--depth', '1', url, str(work / name)],
                       check=True, capture_output=True)


def strip_comments(s: str) -> str:
    s = re.sub(r'/\*.*?\*/', '', s, flags=re.S)
    return re.sub(r'(?m)//.*$', '', s)


def build_corpus(work: pathlib.Path) -> list:
    items = []

    # Strudel's own docs, tunes and examples.
    root = work / 'strudel'
    for p in root.rglob('*'):
        if 'node_modules' in p.parts or not p.is_file():
            continue
        if p.suffix not in ('.mdx', '.astro', '.jsx', '.tsx', '.md'):
            continue
        text = p.read_text(errors='replace')
        for m in re.finditer(r'tune=\{`(.*?)`\}', text, re.S):
            items.append(('docs', str(p.relative_to(root)), m.group(1)))
        if p.suffix == '.mdx':
            for m in re.finditer(r'```(?:js|javascript)?\n(.*?)```', text, re.S):
                if re.search(r'\b(s|n|note|sound|stack|seq|cat)\s*\(', m.group(1)):
                    items.append(('docs', str(p.relative_to(root)), m.group(1)))
    for rel in ('website/src/repl/tunes.mjs', 'website/src/examples.mjs',
                'examples/codemirror-repl/tunes.mjs', 'bench/tunes.bench.mjs'):
        p = root / rel
        if p.exists():
            for m in re.finditer(r'`([^`]{40,})`', p.read_text(errors='replace'), re.S):
                items.append(('docs', rel, m.group(1)))

    # The community song collection.
    sroot = work / 'songs'
    for p in sroot.rglob('*.js'):
        items.append(('songs', str(p.relative_to(sroot)), p.read_text(errors='replace')))

    # Full songs embedded as base64 in strudel.cc links.
    readme = (work / 'awesome' / 'README.md').read_text(errors='replace')
    for m in re.finditer(r'strudel\.cc/[^)\s]*#([A-Za-z0-9%+/=_-]{200,})', readme):
        raw = urllib.parse.unquote(m.group(1)).replace('-', '+').replace('_', '/')
        raw += '=' * (-len(raw) % 4)
        try:
            code = base64.b64decode(raw).decode('utf-8', errors='replace')
        except Exception:
            continue
        if 'sound(' in code or 'note(' in code:
            items.append(('songs', f'awesome#{len(items)}', code))

    seen, uniq = set(), []
    for src, origin, code in items:
        key = code.strip()
        if len(key) >= 10 and key not in seen:
            seen.add(key)
            uniq.append({'src': src, 'origin': origin, 'code': key})
    return uniq


# Classification: 1 representable, 2 build-time helper, 3 new IR node,
# 4 external input, 5 query-time callback, 6 setup only, 7 out of scope.
FEATURES = {
    3: {
        'select / join family':      r'\.\s*(pick|pickF|pickOut|pickmod\w*|pickRestart|pickReset|squeeze|inhabit|innerJoin|outerJoin|squeezeJoin)\s*\(',
        'scales / modes':            r'\.\s*(scale|scaleTranspose|mode)\s*\(',
        'chords & voicing':          r'\.\s*(chord|voicing|anchor|dict|rootNotes|drop)\s*\(|\bchord\s*\(',
        'tuple / multi-value':       r'\.\s*(split|collect)\s*\(|Array\.isArray',
        'arpeggiation':              r'\.\s*arp\w*\s*\(',
    },
    2: {
        'user-defined helpers':      r'\bregister\s*\(',
        'section arrangement':       r'\barrange\s*\(|\bseqPLoop\s*\(',
    },
    4: {
        'external MIDI':             r'\bmidin\s*\(|WebMidi|navigator\.requestMIDI|\.midi\s*\(',
        'keyboard / DOM':            r'addEventListener|keystatus|onkeydown',
    },
    5: {
        'per-event callback':        r'\.\s*(fmap|withValue|withHap|withHaps|filterValues|filterHaps)\s*\(',
        'cross-query mutable state': r'markovstates|hap\.context\s*\.\s*\w+\s*=',
    },
    6: {
        'sample loading / banks':    r'\bsamples\s*\(|\.\s*bank\s*\(',
        'visualisation':             r'\.\s*(color|pianoroll|_pianoroll|_scope|scope|punchcard|_punchcard)\s*\(',
    },
    7: {
        'hydra / csound / async':    r'\bhydra\b|initHydra|\bcsound\b|\bawait\b|new Promise',
    },
}

# Ambient impurity: expected to be ~zero in real songs. That is the finding.
IMPURITY = {
    'Math.random':          r'Math\s*\.\s*random',
    'wall clock':           r'Date\s*\.\s*now|performance\s*\.\s*now|new Date',
    'eval / Function ctor': r'\beval\s*\(|new Function',
    'network fetch':        r'\bfetch\s*\(|XMLHttpRequest',
    'query-time array gen': r'(fmap|withValue|withHap)\s*\([^)]{0,200}(\.map\s*\(|Object\.fromEntries|\.push\s*\()',
}

# Closure argument positions: applied once while building (fine) vs per event (fatal).
BUILD_RATE = {
    'every', 'when', 'off', 'sometimes', 'sometimesBy', 'someCycles', 'often',
    'rarely', 'almostNever', 'almostAlways', 'jux', 'juxBy', 'superimpose',
    'layer', 'apply', 'chunk', 'ply', 'firstOf', 'lastOf', 'iter', 'echoWith',
    'stutWith', 'within', 'linger', 'all', 'register', 'arrange',
}
QUERY_RATE = {
    'fmap', 'withValue', 'withHap', 'withHaps', 'filterValues', 'filterHaps',
    'bind', 'innerBind', 'outerBind', 'collect',
}


def main() -> None:
    work = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else '_work').resolve()
    clone(work)
    corpus = build_corpus(work)
    songs = [c for c in corpus if c['src'] == 'songs']
    docs = [c for c in corpus if c['src'] == 'docs']
    chars = sum(len(c['code']) for c in corpus)

    print(f'corpus: {len(corpus)} snippets, {chars:,} chars '
          f'({len(songs)} song files, {len(docs)} doc snippets)\n')
    print('CAVEAT: the song corpus is overwhelmingly one author; the doc corpus is')
    print('pedagogical one-liners. Trust findings that appear in BOTH columns.\n')

    print(f'{"class / feature":38s} {"songs":>10s} {"docs":>8s}')
    print('-' * 60)
    for cls in sorted(FEATURES):
        for feat, rx in FEATURES[cls].items():
            r = re.compile(rx)
            ns = sum(1 for c in songs if r.search(strip_comments(c['code'])))
            nd = sum(1 for c in docs if r.search(strip_comments(c['code'])))
            print(f'  {cls}  {feat:33s} {ns:4d}/{len(songs):<5d} {nd:8d}')

    print(f'\n{"ambient impurity (expect ~0)":38s} {"songs":>10s} {"docs":>8s}')
    print('-' * 60)
    for name, rx in IMPURITY.items():
        r = re.compile(rx)
        ns = sum(1 for c in songs if r.search(strip_comments(c['code'])))
        nd = sum(1 for c in docs if r.search(strip_comments(c['code'])))
        print(f'     {name:33s} {ns:4d}/{len(songs):<5d} {nd:8d}')

    # Where closures actually sit.
    sites = Counter()
    bodies = defaultdict(list)
    for it in corpus:
        code = strip_comments(it['code'])
        for m in re.finditer(r'(=>|\bfunction\s*\()', code):
            depth, i, owner = 0, m.start() - 1, None
            while i > 0 and m.start() - i < 400:
                ch = code[i]
                if ch in ')]}':
                    depth += 1
                elif ch in '([{':
                    if depth == 0:
                        mm = re.search(r'([A-Za-z_$][\w$]*)\s*$', code[:i])
                        owner = mm.group(1) if (ch == '(' and mm) else '(literal)'
                        break
                    depth -= 1
                i -= 1
            owner = owner or '(top level)'
            sites[owner] += 1
            if owner in QUERY_RATE:
                bodies[it['origin']].append(owner)

    print('\nCLOSURE SITES')
    print('-' * 60)
    for owner, n in sites.most_common(18):
        kind = ('QUERY-RATE — not representable' if owner in QUERY_RATE
                else 'build-time' if owner in BUILD_RATE else '?')
        print(f'  {n:4d}  {owner:24s} {kind}')

    print(f'\nsnippets with a query-rate closure: {len(bodies)} / {len(corpus)}')
    print('(read the bodies before believing this number — most are userland')
    print(' workarounds for missing features, not genuine dynamism)')

    (work / 'corpus.json').write_text(json.dumps(corpus, indent=1))
    print(f'\nwrote {work / "corpus.json"}')


if __name__ == '__main__':
    main()
