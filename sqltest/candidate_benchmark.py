"""Candidate-only CLI timing against an immutable existing C baseline bundle."""
import json
from pathlib import Path
from datetime import datetime,timezone
import time
from support import ROOT,out,save,digest,sources
from freeze_inputs import verify,DATA
from compare_cli import baseline_bundle
from benchmark import dispatch,environment_record
from analysis import stats,key


def summary(rows):
    groups={}
    for row in rows:
        if row['status']=='PASS':groups.setdefault(row['family']+'/'+row['case']+'/'+str(row['seed']),[]).append(row)
    return {name:{'samples':len(items),'candidate_total_ns':stats([r['ns'] for r in items]),'reference_C_total_ns':stats([r['reference_ns'] for r in items]),'reference_over_candidate_ratio':stats([r['reference_ns']/r['ns'] for r in items])} for name,items in sorted(groups.items())}

def compare(binary,output,baseline):
    m=verify();cfg=m['config'];baseline=Path(baseline).resolve();reference=baseline_bundle(baseline,m);binary=Path(binary).resolve()
    if not binary.is_file():raise ValueError('candidate binary required')
    output=out(Path(output));output.mkdir(exist_ok=False)
    selected=[r for r in reference['samples'] if r['family']=='uuid-query' or (r['family']=='ingest' and r['case'].startswith('cli-'))]
    reference_map={key(r):r for r in selected}
    if len(reference_map)!=126:raise ValueError('C CLI baseline matrix incomplete')
    state={'status':'RUNNING','reviewed_commit':reference['reviewed_commit'],'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'sources':sources(),'config':cfg,'binary_sha256':digest(binary),'reference_bundle':str(baseline),'reference_started_utc':reference['started_utc'],'started_utc':datetime.now(timezone.utc).isoformat(),'environment':environment_record(cfg['cpu']),'samples':[],'API':'NOT_RUN: compatible native API backend required','LIVE':'NOT_RUN: same-run native API backend required','core_API_catalog':'NOT_RUN: matching API result/error contract required','C_loaded_by_comparator':False,'baseline_audit':'PASS','warmups':[]}
    save(output/'status.json',state);deadline=time.monotonic()+cfg['full_timeout_seconds'];warm=set()
    try:
        order=[{'family':'ingest',**json.loads(x)} for x in (DATA/'order.txt').read_text().splitlines() if json.loads(x)['case'].startswith('cli-')]
        order += [{'family':'uuid-query',**json.loads(x)} for x in (DATA/'uuid-query-order.txt').read_text().splitlines()]
        for n,row in enumerate(order):
            ref=reference_map[key(row)];seed=row['seed'];label='single' if row['case'].endswith('auto') else 'full'
            data=m['datasets'][f'uuid7-{seed}-full' if row['family']=='uuid-query' else f'{seed}-{label}']
            job={**row,'mode':'timed','engine':'candidate','binary':str(binary),'binary_sha256':digest(binary),'expected_rows':ref['rows'],'reference_canonical_sha256':ref['canonical']['sha256'],'dataset_folder':data['folder'],'reviewed_commit':state['reviewed_commit'],'manifest_sha256':state['input_manifest_sha256'],'run_state':str(output/'status.json'),'output':str(output/f'sample-{n:04}'),'cpu':cfg['cpu']}
            wk=(row['family'],row['case'],seed,row['session'])
            if wk not in warm:
                result=dispatch({**job,'mode':'untimed','output':str(output/f'warm-{n:04}')},timeout=cfg['case_timeout_seconds'],deadline=deadline,sandbox_limit=(output,cfg['sandbox_budget_bytes']),permit_failure=True);warm.add(wk);state['warmups'].append({'key':list(wk),'status':result['status'],'result_path':str((output/f'warm-{n:04}'/'result.json').relative_to(ROOT))})
            result=dispatch(job,timeout=cfg['case_timeout_seconds'],deadline=deadline,sandbox_limit=(output,cfg['sandbox_budget_bytes']),permit_failure=True)
            result['result_path']=str((Path(job['output'])/'result.json').relative_to(ROOT));result['reference_ns']=ref['ns'];state['samples'].append(result);save(output/'status.json',state)
        if {key(r) for r in state['samples']}!=set(reference_map) or len(state['samples'])!=126:raise ValueError('candidate matrix missing/duplicate')
        statistics=summary(state['samples'])
        baseline_env=reference.get('timing_environment',reference['environment']);now_env=state['environment'];fields=cfg['historical_compare']['runtime_fields']
        comparable=all(baseline_env.get(k)==now_env.get(k) for k in fields)
        load_ok=abs(baseline_env['load'][0]-now_env['load'][0])<=cfg['historical_compare']['max_load1_delta']
        comparable=comparable and load_ok
        now_env['candidate_build_provenance']='user binary SHA only; compiler/flags not provided'
        now_env['baseline_protocol_C_flags']=now_env.pop('C_flags')
        state['comparable_environment']=comparable;state['environment_mismatches']=[k for k in fields if baseline_env.get(k)!=now_env.get(k)]+([] if load_ok else ['load1_delta'])
        state['ratio_label']='historical diagnostic only' if not comparable else 'historical same-family descriptive ratio; not causal A/B'
        state.update({'status':'PASS_SCOPED' if all(r['status']=='PASS' for r in state['samples']) and all(r['status']=='PASS' for r in state['warmups']) else 'DIFF_OR_INCOMPLETE','statistics':statistics,'ratio_caveat':'historical C vs later candidate; not contemporaneous A/B; compare environment/config and same CLI family only','finished_utc':datetime.now(timezone.utc).isoformat()})
        save(output/'status.json',state)
        import csv
        with out(output/'times.csv').open('w',newline='') as f:
            w=csv.writer(f);w.writerow(['family','case','seed','session','repeat','status','reference_C_ns','candidate_ns'])
            for r in state['samples']:w.writerow([r['family'],r['case'],r['seed'],r['session'],r['repeat'],r['status'],r['reference_ns'],r.get('ns')])
        from audit import audit_candidate
        audit_candidate(output)
        return state
    except BaseException as e:state['status']='FAIL';state['error']=str(e);save(output/'status.json',state);raise
