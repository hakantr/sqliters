#!/usr/bin/env python3
"""One bounded C-only job. Timing requires a reviewed active parent run ticket."""
import csv
import json
import os
from pathlib import Path
import resource
import sys
import time
sys.dont_write_bytecode=True
from support import ROOT,ART,out,save,digest,set_execution_log
from gates import worker as admit


def execute(job):
    from external import one_case,cli,read
    from correctness import run as correctness_run,load_uuid
    from native import Database
    from wireio import normalize_file
    from freeze_inputs import DATA
    m=json.loads((ROOT/'external-input-manifest.json').read_text());timed=job['mode']=='timed';output=out(Path(job['output']));output.mkdir(exist_ok=True)
    build=job['build']
    for name in ['cli','library']:
        if digest(build[name])!=build[name+'_sha256']:raise ValueError('worker binary drift')
    if job['family']=='ingest':return one_case(m,job['case'],job['seed'],output,timed=timed)
    if job['family']=='correctness':
        os.chdir(output);out(output/'artifacts/placeholder');(output/'artifacts').mkdir(exist_ok=True)
        return correctness_run(m,job['spec'],build,output)
    if job['family']=='uuid-query':
        data=ROOT/m['datasets'][f'uuid7-{job["seed"]}-'+('smoke' if job.get('smoke') else 'full')]['folder'];path=output/'uuid.db';db=Database(build['library'],path)
        try:
            from external import configured
            configured(db);load_uuid(db,data)
        finally:db.close()
        leaf='join-cli-index.txt' if job['case']=='join-index' else 'join-cli-noindex.txt';payload=(data/leaf).read_bytes()
        t=time.perf_counter_ns() if timed else None
        result=__import__('support').command([build['cli'],str(path)],input=payload)
        duration=time.perf_counter_ns()-t if timed else None
        out(output/'query.stdout').write_bytes(result.stdout);out(output/'query.stderr').write_bytes(result.stderr)
        if result.stderr:raise ValueError('UUID CLI stderr')
        from wireio import query_payload
        out(output/'query.typed.stdout').write_bytes(query_payload(result.stdout))
        item=normalize_file(output/'query.typed.stdout',output/'canonical.wire',4)
        if item['sha256']!=digest(data/'join-expected.txt') or item['rows']!=m['config']['frozen_uuid7']['smoke_details' if job.get('smoke') else 'details']:raise ValueError('UUID CLI own result mismatch')
        item.update({'path':str((output/'canonical.wire').relative_to(ROOT)),'columns':4,'expected':str((data/'join-expected.txt').relative_to(ROOT))})
        return {'status':'PASS','rows':item['rows'],'ns':duration,'metric':'CLI startup+configuration+query+all output; DB load outside','canonical':item,'row_latency_ns':[],'batch_latency_ns':[]}
    if job['family']=='live':
        from live import run
        return run(build['library'],m['config']['live'],job['uuid_mode'],output,commit_mode=job['commit_mode'],timed=timed,smoke_only=job.get('smoke',False),batch_rows=m['config']['batch_rows'])
    raise ValueError('unknown worker family')


def main():
    job_path=Path(sys.argv[1]);job=json.loads(job_path.read_text());output=out(Path(job['output']));output.mkdir(exist_ok=True);result_path=output/'result.json'
    save(result_path,{'status':'RUNNING','family':job['family'],'mode':job['mode']})
    set_execution_log(output/'processes.jsonl')
    try:
        admit(job)
        cfg=json.loads((ROOT/'external-config.json').read_text())
        if hasattr(os,'sched_setaffinity') and job.get('cpu') is not None:os.sched_setaffinity(0,{job['cpu']})
        resource.setrlimit(resource.RLIMIT_AS,(cfg['rss_budget_bytes'],cfg['rss_budget_bytes']))
        resource.setrlimit(resource.RLIMIT_FSIZE,(cfg['sandbox_budget_bytes'],cfg['sandbox_budget_bytes']))
        load_before=os.getloadavg()
        result=execute(job);result['load_before']=load_before;result['load_after']=os.getloadavg();result.update({'worker_pid':os.getpid(),'worker_argv':sys.argv,'worker_mode':job['mode'],'max_rss_bytes':resource.getrusage(resource.RUSAGE_SELF).ru_maxrss*1024})
        if job['family']!='correctness':
            result.update({k:job[k] for k in ['family','case','session','repeat'] if k in job});result['seed']=job.get('seed')
        save(result_path,result)
    except BaseException as e:
        import traceback
        out(output/'exception.txt').write_text(traceback.format_exc());save(result_path,{'status':'RESOURCE' if isinstance(e,MemoryError) else 'FAIL','family':job['family'],'mode':job['mode'],'worker_pid':os.getpid(),'error':str(e),'error_type':type(e).__name__,'sql_error':{k:getattr(e,k,None) for k in ['code','extended','message','offset','stage','cleanup_finalize_rc']},'job_id':job.get('case',job.get('spec',{}).get('id'))})
        raise

if __name__=='__main__':main()
