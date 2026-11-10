"""Shared filesystem/provenance helpers; every output stays in sqltest/artifacts."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent
ART = ROOT / 'artifacts/external'


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for block in iter(lambda: stream.read(1048576), b''):
            h.update(block)
    return h.hexdigest()


def out(path):
    path = Path(path)
    if not path.resolve().is_relative_to(ROOT) or any(p.is_symlink() for p in [path, *path.parents]):
        raise ValueError(f'outside/symlink output: {path}')
    path.parent.mkdir(parents=True, exist_ok=True)
    return path


def save(path, value):
    out(path).write_text(json.dumps(value, ensure_ascii=False, indent=2, sort_keys=True) + '\n')


def environment():
    tmp = ART / 'tmp'
    out(tmp / 'placeholder'); tmp.mkdir(exist_ok=True)
    return {'PATH':'/usr/bin:/bin', 'LANG':'C', 'LC_ALL':'C', 'TZ':'UTC',
            'TMPDIR':str(tmp), 'SQLITE_TMPDIR':str(tmp), 'CC':'/usr/bin/cc',
            'CFLAGS':'-O2', 'CPPFLAGS':'', 'LDFLAGS':'', 'LIBS':'', 'MAKEFLAGS':'', 'MFLAGS':''}


def command(args, *, cwd=None, timeout=600, input=None):
    return subprocess.run([str(x) for x in args], cwd=cwd or ROOT, env=environment(),
                          input=input, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                          timeout=timeout, check=True)


def sources():
    names = ['support.py','sqlitebuild.py','native.py','freeze_inputs.py','external.py','harness_controls.py',
             'external-config.json','uuid_family.py','live.py','docs/external-baseline-protokolu.md']
    return {name:digest(ROOT/name) for name in names}
