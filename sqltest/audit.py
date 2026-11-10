#!/usr/bin/env python3
"""Independent read-only raw/result/statistics audit. Never loads a C/Rust library."""
import hashlib
import json
from pathlib import Path
import sys
sys.dont_write_bytecode=True
from datetime import datetime
from support import ROOT,digest,sources
from freeze_inputs import verify
from analysis import summarize,expected_keys,key
from correctness import plan
from wireio import inspect,normalize_row


def artifact(item):
    path=ROOT/item['path']
    if not path.is_file():raise ValueError('missing canonical artifact')
    if item.get('format')=='variable-wire':
        rows=fields=0
        with path.open('rb') as f:
            for line in f:
                width,raw=line.split(b'\t',1);n=int(width)
                if n<1 or normalize_row(raw,n)!=raw:raise ValueError('bad variable typed wire')
                rows+=1;fields+=n
        actual={'rows':rows,'fields':fields,'sha256':digest(path)}
    else:actual=inspect(path,item.get('columns',item['fields']//item['rows'] if item['rows'] else 7))
    if any(actual[x]!=item[x] for x in ['rows','fields','sha256']):raise ValueError('canonical shape/hash mismatch')
    if item.get('expected') and digest(ROOT/item['expected'])!=actual['sha256']:raise ValueError('golden mismatch/missing')
    if item.get('expected_parts'):
        h=hashlib.sha256()
        for name in item['expected_parts']:
            with (ROOT/name).open('rb') as stream:
                for block in iter(lambda:stream.read(1048576),b''):h.update(block)
        if h.hexdigest()!=actual['sha256']:raise ValueError('append golden mismatch')
    return actual


def live_record(row,cfg):
    from live import validate_recorded
    events=[json.loads(line) for line in (ROOT/row['event_log']).read_text().splitlines()]
    snapshot=json.loads((ROOT/row['readback_path']).read_text())
    result=validate_recorded(snapshot,events,datetime.fromisoformat(row['start_utc']),datetime.fromisoformat(row['end_utc']),cfg['parents'],cfg['details'],cfg['timestamp_tolerance_seconds'])
    if row['rows']!=cfg['parents']+cfg['details'] or len(events)!=row['rows']:raise ValueError('LIVE expected event count')
    if row['payload_generation_ns']!=sum(e['generation_ns'] for e in events):raise ValueError('LIVE generation aggregate')
    if row['db_write_ns']!=sum(e['db_step_ns'] for e in events)+sum(row['commit_latency_ns'])+sum(row['begin_latency_ns']):raise ValueError('LIVE DB time aggregate')
    if row['commit_mode']=='autocommit':
        if row['row_latency_ns']!=[e['db_step_ns'] for e in events] or row['batch_latency_ns'] or row['commit_latency_ns']:raise ValueError('LIVE row commit latency labels')
    else:
        expected_batches=(cfg['parents']+999)//1000+(cfg['details']+999)//1000
        if row['row_latency_ns'] or len(row['batch_latency_ns'])!=expected_batches or len(row['commit_latency_ns'])!=expected_batches:raise ValueError('LIVE batch accounting')
    if row['mode']=='db-default-uuid4-utc' and row['generation_ns'] is not None:raise ValueError('DB UUID generation cannot be isolated')
    # No event/readback SHA equality is ever used as a LIVE value oracle.
    return result


def audit(folder,*,preflight=False):
    folder=Path(folder);m=verify();state=json.loads((folder/'status.json').read_text());cfg=m['config']
    if state.get('status')!='PASS_SCOPED' or state['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json') or state['sources']!=sources() or state['config']!=cfg:raise ValueError('run metadata/source/config mismatch')
    if state.get('full_correctness_gate')!='PASS':raise ValueError('full correctness gate missing')
    rows=[]
    for entry in state['samples']:
        path=ROOT/entry['result_path'];row=json.loads(path.read_text())
        if row!={k:v for k,v in entry.items() if k!='result_path'}:raise ValueError('index/raw result mismatch')
        launch=json.loads((path.parent/'launch.json').read_text());job=json.loads((path.parent/'job.json').read_text())
        if launch['exit_code']!=0 or launch['pid']!=row['worker_pid'] or not launch.get('new_session') or row['worker_mode']!='timed' or job['mode']!='timed':raise ValueError('worker PID/exit/mode')
        if key(job)!=key(row) or job['reviewed_commit']!=state['reviewed_commit'] or row['status']!='PASS':raise ValueError('worker matrix/provenance')
        for name in ['library','cli']:
            if job['build'][name+'_sha256']!=state['builds']['2000'][name+'_sha256']:raise ValueError('worker build identity')
        if row['ns'] is None or row['ns']<=0:raise ValueError('missing duration')
        if row['family']=='live':live_record(row,cfg['live'])
        else:
            item=artifact(row['canonical'])
            if row.get('raw_csv'):artifact(row['raw_csv'])
            if row['family']=='ingest':
                expected_rows=cfg['single_rows'] if row['case'].endswith('auto') else cfg['normal_rows']
                if row['rows']!=expected_rows or item['fields']!=expected_rows*7:raise ValueError('ingest shape')
                if row['case'].startswith('api'):
                    count=expected_rows if row['case'].endswith('auto') else 0
                    if len(row['row_latency_ns'])!=count:raise ValueError('real row latency count')
                    if row['case']=='api-prepared-batch' and len(row['batch_latency_ns'])!=(expected_rows+cfg['batch_rows']-1)//cfg['batch_rows']:raise ValueError('batch latency count')
                    if row['case']=='api-prepared-one-txn' and len(row['batch_latency_ns'])!=1:raise ValueError('transaction latency count')
            elif row['rows']!=cfg['frozen_uuid7']['details'] or item['fields']!=row['rows']*4:raise ValueError('UUID full result coverage')
        rows.append(row)
    warmkeys=set()
    for relative in state.get('warmups',[]):
        p=ROOT/relative;w=json.loads(p.read_text());job=json.loads((p.parent/'job.json').read_text());launch=json.loads((p.parent/'launch.json').read_text())
        k=(job['family'],job['case'],job.get('seed'),job['session'])
        if k in warmkeys or job['mode']!='untimed' or w['ns'] is not None or launch['exit_code']!=0:raise ValueError('warmup duplicated/missing/actually timed')
        warmkeys.add(k)
    if warmkeys!={(r['family'],r['case'],r.get('seed'),r['session']) for r in rows}:raise ValueError('warmup matrix')
    stats=summarize(rows,cfg)
    if stats!=state['statistics']:raise ValueError('statistics not raw-recomputed')
    ids=[x['id'] for x in state['correctness']];wanted={x['id'] for x in plan(m)}
    if len(ids)!=len(set(ids)) or set(ids)!=wanted:raise ValueError('correctness catalog matrix incomplete')
    replacements=state.get('coverage_replacements',[])
    if len(replacements)!=3 or any(x['replaced_by'] not in wanted or x['status']!='SUPERSEDED_BY_TYPED_CSV' for x in replacements):raise ValueError('legacy wide replacement map missing')
    for index in state['correctness']:
        path=ROOT/index['result_path'];row=json.loads(path.read_text());launch=json.loads((path.parent/'launch.json').read_text())
        if row['status'] not in ['PASS','EXPECTED_ERROR'] or launch['exit_code']!=0 or row['id']!=index['id']:raise ValueError('correctness failure/skip')
        for e in row.get('errors',[]):
            if not all(k in e for k in ['primary','extended','message','offset','stage','autocommit']):raise ValueError('error fields missing')
        if row['status']=='EXPECTED_ERROR' and not row['errors']:raise ValueError('expected error lacks actual evidence')
        for item in row.get('artifacts',[]):artifact(item)
    if not any('wal' in x['case'] for x in state['not_run']):raise ValueError('unimplemented WAL must remain explicit')
    print(f'PASS scoped audit: {len(rows)} unique raw samples, {len(ids)} correctness cases, typed outputs, same-run LIVE invariants and recomputed stats; recipes remain NOT_RUN')
    return {'samples':len(rows),'correctness_cases':len(ids),'status':'PASS_SCOPED_AUDIT'}

def audit_candidate(folder):
    folder=Path(folder);state=json.loads((folder/'status.json').read_text());m=verify()
    if state.get('status') not in ['PASS_SCOPED','DIFF_OR_INCOMPLETE'] or state['sources']!=sources() or state['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json'):raise ValueError('candidate metadata/source/input')
    reference=json.loads((Path(state['reference_bundle'])/'status.json').read_text());audit(Path(state['reference_bundle']))
    refs={key(r):r for r in reference['samples'] if r['family']=='uuid-query' or (r['family']=='ingest' and r['case'].startswith('cli-'))}
    rows=[]
    for entry in state['samples']:
        path=ROOT/entry['result_path'];raw=json.loads(path.read_text());launch=json.loads((path.parent/'launch.json').read_text());job=json.loads((path.parent/'job.json').read_text())
        if raw!={k:v for k,v in entry.items() if k not in ['reference_ns','result_path']}:raise ValueError('candidate index/raw mismatch')
        if key(raw)!=key(entry) or key(raw)!=key(job) or entry['reference_ns']!=refs[key(raw)]['ns']:raise ValueError('candidate raw/key/reference mismatch')
        if job['binary_sha256']!=state['binary_sha256'] or job.get('engine')!='candidate' or raw.get('C_loaded') is not False:raise ValueError('candidate backend contract')
        if raw['status']=='PASS':
            if launch['exit_code']!=0 or raw['ns'] is None or raw['ns']<=0 or raw['worker_pid']!=launch['pid']:raise ValueError('candidate PID/exit/duration')
            item=artifact(raw['canonical'])
            if item['sha256']!=refs[key(raw)]['canonical']['sha256']:raise ValueError('candidate vs C actual content')
        rows.append(entry)
    if len(rows)!=126 or len({key(r) for r in rows})!=126 or {key(r) for r in rows}!=set(refs):raise ValueError('candidate CLI timing matrix')
    from candidate_benchmark import summary
    if summary(rows)!=state['statistics']:raise ValueError('candidate raw statistics mismatch')
    if state['status']=='PASS_SCOPED' and any(r['status']!='PASS' for r in rows):raise ValueError('candidate failure hidden')
    print('PASS candidate artifact audit: 126 CLI samples; cached C golden comparison and historical statistics; API/LIVE NOT_RUN')
    return {'status':'AUDIT_PASS','candidate_status':state['status']}


if __name__=='__main__':
    if len(sys.argv)>2 and sys.argv[1]=='--candidate':audit_candidate(sys.argv[2])
    else:audit(sys.argv[1])
