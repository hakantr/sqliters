"""Reviewed C-only orchestration. All workloads are frozen, LIVE is explicitly separate."""
import json
import os
from pathlib import Path
import platform
import subprocess
import sys
import time
from datetime import datetime,timezone
from support import ROOT,ART,out,save,digest,sources,environment
from analysis import expected_keys,summarize
from correctness import plan
from gates import admission


def utc():return datetime.now(timezone.utc).isoformat()

def environment_record(cpu):
    from sqlitebuild import FLAGS
    cpuinfo=Path('/proc/cpuinfo').read_text() if Path('/proc/cpuinfo').exists() else ''
    return {'platform':platform.platform(),'python':sys.version,'cpu':cpu,'allowed_cpus':sorted(os.sched_getaffinity(0)) if hasattr(os,'sched_getaffinity') else None,
      'cpuinfo':cpuinfo,'cpu_model':next((line.split(':',1)[1].strip() for line in cpuinfo.splitlines() if line.startswith('model name')),'unknown'),'load':os.getloadavg() if hasattr(os,'getloadavg') else None,'environment':environment(),'C_flags':FLAGS,
      'governor':Path(f'/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor').read_text().strip() if cpu is not None and Path(f'/sys/devices/system/cpu/cpu{cpu}/cpufreq/scaling_governor').exists() else 'unknown',
      'SMT_siblings':Path(f'/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list').read_text().strip() if cpu is not None and Path(f'/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list').exists() else 'unknown','cache':'warm file cache; no cold OS-cache claim'}


def bytes_under(path):return sum(p.stat().st_size for p in Path(path).rglob('*') if p.is_file())


def dispatch(job,*,timeout,deadline=None,sandbox_limit=None,permit_failure=False):
    output=out(Path(job['output']));output.mkdir(exist_ok=False);jobfile=output/'job.json';save(jobfile,job)
    argv=[sys.executable,str(ROOT/('candidate_worker.py' if job.get('engine')=='candidate' else 'external_worker.py')),str(jobfile)]
    with out(output/'worker.stdout').open('wb') as stdout,out(output/'worker.stderr').open('wb') as stderr:
        proc=subprocess.Popen(argv,cwd=ROOT,env=environment(),stdout=stdout,stderr=stderr,start_new_session=True);start=time.monotonic()
        save(output/'launch.json',{'pid':proc.pid,'argv':argv,'started_utc':utc(),'timeout_seconds':timeout,'process_group':proc.pid,'new_session':True})
        failure=None
        while proc.poll() is None:
            if time.monotonic()-start>timeout:failure='case timeout';break
            if deadline is not None and time.monotonic()>deadline:failure='global timeout';break
            if sandbox_limit and bytes_under(sandbox_limit[0])>sandbox_limit[1]:failure='sandbox disk budget exceeded';break
            time.sleep(.2)
        if failure:
            # Worker and its CLI descendants stay in the worker process group.
            # Child CLI processes are bounded by their own communicate timeout;
            # parent additionally terminates the owned worker process group.
            import signal
            try:os.killpg(proc.pid,signal.SIGKILL)
            except ProcessLookupError:pass
            proc.kill();proc.wait()
        else:proc.wait()
    launch=json.loads((output/'launch.json').read_text());launch.update({'exit_code':proc.returncode,'finished_utc':utc(),'failure':failure});save(output/'launch.json',launch)
    if failure:save(output/'result.json',{'status':'TIMEOUT' if 'timeout' in failure else 'RESOURCE','error':failure,'worker_pid':proc.pid,'worker_mode':job['mode'],'C_loaded':False if job.get('engine')=='candidate' else None,**{k:job[k] for k in ['family','case','seed','session','repeat'] if k in job}})
    path=output/'result.json'
    result=json.loads(path.read_text()) if path.exists() else {'status':'HARNESS_ERROR','error':'worker result missing'}
    if proc.returncode and result.get('status')=='PASS':result={'status':'HARNESS_ERROR','error':'PASS from nonzero child'}
    if permit_failure:
        result.update({k:job[k] for k in ['family','case','seed','session','repeat'] if k in job});return result
    if result.get('status') not in ['PASS','EXPECTED_ERROR','NOT_RUN']:raise RuntimeError(f'{job.get("case",job.get("spec",{}).get("id"))}: {result}')
    return result


def planned_jobs(m,builds,output,commit):
    cfg=m['config'];order=[json.loads(x) for x in (ART/'inputs/order.txt').read_text().splitlines()]
    order += [{'family':'uuid-query',**json.loads(x)} for x in (ART/'inputs/uuid-query-order.txt').read_text().splitlines()]
    order += [{'family':'live',**json.loads(x)} for x in (ART/'inputs/live-order.txt').read_text().splitlines()]
    result=[]
    for i,row in enumerate(order):
        family=row.get('family','ingest');case=row['case'];seed=row.get('seed');job={'mode':'timed','family':family,**row,'reviewed_commit':commit,'run_state':str(output/'status.json'),'manifest_sha256':digest(ROOT/'external-input-manifest.json'),'build':builds['2000'],'output':str(output/f'sample-{i:04}'),'cpu':cfg['cpu']}
        if family=='ingest':job['dataset_folder']=m['datasets'][f'{seed}-'+('single' if case.endswith('auto') else 'full')]['folder']
        elif family=='uuid-query':job['dataset_folder']=m['datasets'][f'uuid7-{seed}-full']['folder']
        else:job['dataset_folder']=str((ART/'inputs/live').relative_to(ROOT))
        result.append(job)
    from analysis import key
    if {key(j) for j in result}!=expected_keys(cfg) or len(result)!=len(expected_keys(cfg)):raise ValueError('job matrix mismatch')
    return result


def _run_admitted(commit,output):
    m,builds=admission(commit);cfg=m['config'];output=out(Path(output));output.mkdir(exist_ok=False)
    state={'status':'RUNNING','reviewed_commit':commit,'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'sources':sources(),'config':cfg,'builds':builds,'started_utc':utc(),'environment':environment_record(cfg['cpu']),'samples':[],'correctness':[],'not_run':[{'case':x['id'],'reason':'recipe only; no C orchestrator/API backend'} for x in m['catalog_additional_cases'] if x['family'] in ['wal','api_recipe','crash_recipe','application_recipe']]}
    state['coverage_replacements']=[{'case':x['id'],'replaced_by':'wide-'+str(x['future_max_column']),'status':'SUPERSEDED_BY_TYPED_CSV','reason':'legacy CTE loader not executed; full CSV + frozen mutation + typed golden worker covers actual operation'} for x in m['catalog_additional_cases'] if x['id'].startswith('wide-')]
    save(ROOT/'results/external-summary.json',{'status':'RUNNING','run_path':str(output.relative_to(ROOT)),'reviewed_commit':commit});save(output/'status.json',state);deadline=time.monotonic()+cfg['full_timeout_seconds']
    try:
        jobs=planned_jobs(m,builds,output,commit)
        # Full untimed correctness is a mandatory gate BEFORE any warmup/timing.
        for n,spec in enumerate(plan(m)):
            job={'mode':'untimed','family':'correctness','spec':spec,'build':builds[str(spec['maximum'])],'output':str(output/f'correctness-{n:04}'),'cpu':cfg['cpu']}
            state['active_job']={'family':job['family'],'case':spec['id'],'result_path':str((Path(job['output'])/'result.json').relative_to(ROOT))};save(output/'status.json',state)
            row=dispatch(job,timeout=cfg['case_timeout_seconds'],deadline=deadline,sandbox_limit=(output,cfg['sandbox_budget_bytes']));state['correctness'].append({'id':spec['id'],'result_path':str((Path(job['output'])/'result.json').relative_to(ROOT)),'status':row['status']});save(output/'status.json',state)
        from freeze_inputs import verify
        verify()
        state['full_correctness_gate']='PASS';state['timing_environment']=environment_record(cfg['cpu']);save(output/'status.json',state)
        for n,job in enumerate(jobs):
            warm_key=(job['family'],job['case'],job.get('seed'),job['session'])
            if not any(tuple(x)==warm_key for x in state.get('warmup_keys',[])):
                state['active_job']={'family':job['family'],'case':job['case'],'phase':'warmup','result_path':str((output/f'warm-{n:04}'/'result.json').relative_to(ROOT))};save(output/'status.json',state)
                warm={**job,'mode':'untimed','output':str(output/f'warm-{n:04}')};dispatch(warm,timeout=cfg['case_timeout_seconds'],deadline=deadline,sandbox_limit=(output,cfg['sandbox_budget_bytes']));state.setdefault('warmup_keys',[]).append(warm_key);state.setdefault('warmups',[]).append(str((Path(warm['output'])/'result.json').relative_to(ROOT)))
            state['active_job']={'family':job['family'],'case':job['case'],'phase':'timed','result_path':str((Path(job['output'])/'result.json').relative_to(ROOT))};save(output/'status.json',state)
            row=dispatch(job,timeout=cfg['case_timeout_seconds'],deadline=deadline,sandbox_limit=(output,cfg['sandbox_budget_bytes']));row['result_path']=str((Path(job['output'])/'result.json').relative_to(ROOT));state['samples'].append(row);save(output/'status.json',state)
        from freeze_inputs import verify
        verify();state['statistics']=summarize(state['samples'],cfg);state['status']='PASS_SCOPED';state['finished_utc']=utc();state['load_end']=os.getloadavg();save(output/'status.json',state)
        from audit import audit
        audit(output)
        small={k:state[k] for k in ['status','reviewed_commit','input_manifest_sha256','started_utc','finished_utc','statistics','not_run']};small.update({'samples':len(state['samples']),'correctness_cases':len(state['correctness']),'run_path':str(output.relative_to(ROOT))});save(ROOT/'results/external-summary.json',small)
    except BaseException as e:
        state['status']='FAIL';state['error']=str(e);state['finished_utc']=utc();save(output/'status.json',state);save(ROOT/'results/external-summary.json',{'status':'FAIL','run_path':str(output.relative_to(ROOT)),'error':str(e)});raise


def run(commit,output):
    # Even admission failure belongs to a NEW failed attempt, never stale PASS.
    output=out(Path(output))
    if output.exists():raise ValueError('fresh output directory required; old bundles never overwritten')
    pointer=ROOT/'results/external-summary.json'
    save(pointer,{'status':'RUNNING','run_path':str(output.relative_to(ROOT)),'reviewed_commit':commit})
    try:return _run_admitted(commit,output)
    except BaseException as e:
        output.mkdir(exist_ok=True)
        if not (output/'status.json').exists():save(output/'status.json',{'status':'FAIL','scope':'admission','error':str(e),'reviewed_commit':commit})
        save(pointer,{'status':'FAIL','run_path':str(output.relative_to(ROOT)),'reviewed_commit':commit,'error':str(e)})
        raise
