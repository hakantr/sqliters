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
    path=path.resolve()
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
    argv=[str(x) for x in args]
    proc=subprocess.Popen(argv,cwd=cwd or ROOT,env=environment(),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    record_process(argv,proc.pid)
    try:
        stdout,stderr=proc.communicate(input=input,timeout=timeout)
    except BaseException:
        proc.kill();proc.communicate();record_process(argv,proc.pid,proc.returncode);raise
    record_process(argv,proc.pid,proc.returncode)
    result=subprocess.CompletedProcess(argv,proc.returncode,stdout,stderr)
    if proc.returncode:raise subprocess.CalledProcessError(proc.returncode,argv,stdout,stderr)
    return result


def sources():
    names=[p.name for p in sorted(ROOT.glob('*.py'))]+['external-config.json','docs/external-baseline-protokolu.md']
    return {name:digest(ROOT/name) for name in names}

EXECUTION_LOG=None

def set_execution_log(path):
    global EXECUTION_LOG
    EXECUTION_LOG=out(path) if path is not None else None

def record_process(args,pid,returncode=None):
    if EXECUTION_LOG is not None:
        from datetime import datetime,timezone
        with out(EXECUTION_LOG).open('a') as f:
            f.write(json.dumps({'argv':[str(x) for x in args],'pid':pid,'returncode':returncode,'utc':datetime.now(timezone.utc).isoformat()})+'\n')
