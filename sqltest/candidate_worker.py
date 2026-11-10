#!/usr/bin/env python3
"""Candidate CLI only. No native Database construction or C library loading."""
import json
import os
from pathlib import Path
import resource
import sys
import time
sys.dont_write_bytecode=True
from support import ROOT,out,save,digest,set_execution_log,command
from freeze_inputs import DATA
from gates import worker as admission
from compare_cli import own,classify


def execute(job):
    from external import cli,persistent_cli
    m=json.loads((ROOT/'external-input-manifest.json').read_text());binary=Path(job['binary']);output=out(Path(job['output']));output.mkdir(exist_ok=True);db=output/'candidate.db';timed=job['mode']=='timed'
    if digest(binary)!=job['binary_sha256']:raise ValueError('candidate binary drift')
    data=ROOT/job['dataset_folder'];family=job['family'];case=job['case'];build={'cli':str(binary)}
    if family=='uuid-query':
        cli(build,db,data/'load-cli.txt',output/'load.stdout')
        script=data/('join-cli-index.txt' if case=='join-index' else 'join-cli-noindex.txt');payload=script.read_bytes()
        start=time.perf_counter_ns() if timed else None
        raw=command([binary,str(db)],input=payload)
        duration=time.perf_counter_ns()-start if timed else None
        out(output/'query.stdout').write_bytes(raw.stdout);out(output/'query.stderr').write_bytes(raw.stderr)
        if raw.stderr:raise ValueError('candidate CLI query stderr: '+raw.stderr.decode(errors='replace'))
        from wireio import normalize_file
        from wireio import query_payload
        out(output/'query.typed.stdout').write_bytes(query_payload(raw.stdout))
        result=normalize_file(output/'query.typed.stdout',output/'canonical.wire',4);expected=data/'join-expected.txt'
    else:
        mode='import' if case=='cli-import' else 'read-'+case.split('-')[-1]
        if case.startswith('cli-persistent'):
            duration,acks=persistent_cli(binary,db,data,case.split('-')[-1],timed=timed)
            if not acks or acks[-1].strip()!=str(job['expected_rows']).encode():raise ValueError('candidate persistent ACK count')
        else:
            start=time.perf_counter_ns() if timed else None
            response=cli(build,db,data/(mode+'.txt'),output/'ingest.stdout')
            duration=time.perf_counter_ns()-start if timed else None
            if response.stdout.splitlines()[-3:]!=[b'wal',b'1',b'0']:raise ValueError('candidate connection settings')
        if case=='cli-import':
            raw=own(binary,db,DATA/'cli-raw-verify.txt',output/'raw.stdout',output/'raw.wire',11)
            if raw['sha256']!=digest(data/'raw-expected.txt'):raise ValueError('candidate raw CSV fields differ')
            cli(build,db,DATA/'cli-transform-only.txt',output/'transform.stdout')
        result=own(binary,db,DATA/'cli-verify.txt',output/'verify.stdout',output/'canonical.wire',7);expected=data/'expected.txt'
    if result['rows']!=job['expected_rows'] or result['sha256']!=digest(expected) or result['sha256']!=job['reference_canonical_sha256']:raise ValueError('candidate OWN SELECT differs from actual C golden')
    result.update({'path':str((output/'canonical.wire').relative_to(ROOT)),'expected':str(expected.relative_to(ROOT)),'columns':4 if family=='uuid-query' else 7})
    return {'status':'PASS','rows':result['rows'],'ns':duration,'canonical':result,'row_latency_ns':[],'batch_latency_ns':[],'C_loaded':False}


def main():
    job=json.loads(Path(sys.argv[1]).read_text());output=out(Path(job['output']));output.mkdir(exist_ok=True);path=output/'result.json'
    save(path,{'status':'RUNNING'});set_execution_log(output/'processes.jsonl')
    common={k:job[k] for k in ['family','case','session','repeat']};common.update({'seed':job.get('seed'),'worker_pid':os.getpid(),'worker_mode':job['mode'],'C_loaded':False})
    try:
        admission(job);cfg=json.loads((ROOT/'external-config.json').read_text())
        if hasattr(os,'sched_setaffinity'):os.sched_setaffinity(0,{cfg['cpu']})
        resource.setrlimit(resource.RLIMIT_AS,(cfg['rss_budget_bytes'],cfg['rss_budget_bytes']));resource.setrlimit(resource.RLIMIT_FSIZE,(cfg['sandbox_budget_bytes'],cfg['sandbox_budget_bytes']))
        load_before=os.getloadavg();result=execute(job);result['load_before']=load_before;result['load_after']=os.getloadavg();save(path,{**result,**common})
    except Exception as e:save(path,{'status':classify(e),'error':str(e),'ns':None,**common});raise

if __name__=='__main__':main()
