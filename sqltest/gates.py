"""Reviewed-code and immutable-input gates before any duration is sampled."""
import json
from pathlib import Path
from support import ROOT,ART,command,digest,sources
from freeze_inputs import verify
from sqlitebuild import verified


def admission(commit):
    m=verify();p=json.loads((ROOT/'results/external-preflight.json').read_text())
    if not m['config']['measurement_implementation_ready']:raise ValueError('measurement code not admitted')
    if p.get('status')!='PASS' or p['performance_status']!='NOT_RUN' or p['sources']!=sources() or p['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json') or p['build_sha256']!=digest(ART/'build.json'):raise ValueError('stale/missing preflight')
    controls=json.loads((ROOT/'results/external-controls.json').read_text())
    if controls.get('status')!='PASS' or controls['sources']!=sources() or controls['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json'):raise ValueError('controls missing/stale/failed')
    for name in [*sources(),'external-input-manifest.json']:
        if command(['git','show',commit+':sqltest/'+name],cwd=ROOT.parent).stdout!=(ROOT/name).read_bytes():raise ValueError('unreviewed code/config/input: '+name)
    return m,{str(w):verified(w) for w in m['config']['wide_columns']}


def worker(job):
    if job['mode']=='untimed':return
    m=json.loads((ROOT/'external-input-manifest.json').read_text());state=json.loads(Path(job['run_state']).read_text())
    if state.get('status')!='RUNNING' or state.get('reviewed_commit')!=job['reviewed_commit'] or state.get('input_manifest_sha256')!=digest(ROOT/'external-input-manifest.json') or job['manifest_sha256']!=digest(ROOT/'external-input-manifest.json'):raise ValueError('worker missing active reviewed run')
    if job.get('engine')=='candidate':
        if state.get('baseline_audit')!='PASS':raise ValueError('candidate requires actual C baseline audit')
    elif state.get('full_correctness_gate')!='PASS':raise ValueError('timing before full correctness forbidden')
    if m['sources']!=sources() or not m['config']['measurement_implementation_ready']:raise ValueError('worker source/config drift')
    for e in m['files']:
        # Verify all actual operator/data inputs for this job, and common templates.
        rel=e['path'];path=ROOT/rel
        if '/catalog/' not in rel and (str(Path(rel).parent)==str(Path(job.get('dataset_folder',''))) or str(Path(rel).parent)==str((ART/'inputs').relative_to(ROOT))):
            if path.stat().st_size!=e['bytes'] or digest(path)!=e['sha256']:raise ValueError('worker input drift')
