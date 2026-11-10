#!/usr/bin/env python3
"""External C-only baseline. Preflight never samples latency or calibrates."""
import argparse
import csv
import hashlib
import json
from pathlib import Path
import os
import statistics
import subprocess
import sys
import time
sys.dont_write_bytecode=True
from support import ROOT,ART,out,save,digest,sources,command,environment
from sqlitebuild import verified
from native import Database,SqlError,wire,TextBytes
from freeze_inputs import verify,values,token_value,DATA


def read(path):return Path(path).read_text()
def folder(m,seed,label):return ROOT/m['datasets'][f'{seed}-{label}']['folder']
def decoded(path):
    with Path(path).open(newline='') as stream:
        rows=csv.reader(stream);next(rows)
        return [values(row) for row in rows]

def canonical(db,query,path):
    h=hashlib.sha256();rows=fields=0
    with out(path).open('wb') as stream:
        for row in db.execute(query):
            b=wire(row);stream.write(b);h.update(b);rows+=1;fields+=len(row)
    return {'rows':rows,'fields':fields,'sha256':h.hexdigest(),'path':str(path.relative_to(ROOT))}

def equal_file(a,b):
    if digest(a)!=digest(b):raise ValueError(f'typed/raw output mismatch: {a} vs {b}')

def configured(db):
    db.exec(read(DATA/'settings.txt'))
    result=list(db.execute(read(DATA/'readback.txt')))
    if result!=[[TextBytes(b'wal')],[1],[0]]:raise ValueError('connection pragma readback mismatch: '+str(result))

def verified_db(build,path):return Database(build['library'],path)

def ingest_api(db,data,path,case,*,timed=False,batch_rows=1000):
    decode_start=time.perf_counter_ns() if timed else None
    rows=decoded(data/'rows.csv');sql=read(data/'row-statements.txt').splitlines()
    decode_ns=time.perf_counter_ns()-decode_start if timed else None
    prepare_start=time.perf_counter_ns() if timed else None
    stmt=db.prepare(read(DATA/'insert-template.txt')) if case!='api-exec-auto' else None
    prepare_ns=time.perf_counter_ns()-prepare_start if timed and stmt else None
    row_times=[];batch_times=[]
    begin,commit=read(DATA/'begin.txt'),read(DATA/'commit.txt')
    started=time.perf_counter_ns() if timed else None
    try:
        size=batch_rows if case=='api-prepared-batch' else len(rows) if case=='api-prepared-one-txn' else 1
        transactional=case in ['api-prepared-batch','api-prepared-one-txn']
        for start in range(0,len(rows),size):
            batch_start=time.perf_counter_ns() if timed and transactional else None
            if transactional:db.exec(begin)
            for i in range(start,min(start+size,len(rows))):
                row_start=time.perf_counter_ns() if timed and not transactional else None
                if stmt:stmt.bind(rows[i]);stmt.run();stmt.reset()
                else:db.exec(sql[i])
                if row_start is not None:row_times.append(time.perf_counter_ns()-row_start)
            if transactional:db.exec(commit)
            if batch_start is not None:batch_times.append(time.perf_counter_ns()-batch_start)
        total=time.perf_counter_ns()-started if timed else None
    finally:
        if stmt:stmt.close(ignore_error=sys.exc_info()[0] is not None)
    return {'rows':len(rows),'ns':total,'row_latency_ns':row_times,'batch_latency_ns':batch_times,'input_read_decode_ns':decode_ns,'prepare_ns':prepare_ns,'finalize_scope':'exec-auto per-row includes prepare/finalize; reused prepared finalize outside loop'}

def cli(build,db_path,driver,raw):
    result=command([build['cli'],str(db_path)],input=('.read '+json.dumps(str(driver.relative_to(ROOT)))+'\n').encode())
    out(raw).write_bytes(result.stdout);out(raw.with_suffix('.stderr')).write_bytes(result.stderr)
    if result.stderr:raise ValueError('CLI reported warnings/errors: '+result.stderr.decode('utf-8','replace'))
    return result

def preflight():
    output=ROOT/'results/external-preflight.json';save(output,{'status':'RUNNING','performance_status':'NOT_RUN'})
    try:
        m=verify();checks=[]
        for maximum in [128,2000,4096]:
            b=verified(maximum);data=folder(m,m['config']['seeds'][0],'preflight')
            sandbox=ART/f'preflight/max-{maximum}';out(sandbox/'placeholder');sandbox.mkdir(exist_ok=True)
            # Fresh small DBs only. These are not benchmark repetitions.
            for mode in ['import','import-typed','read-one-txn']:
                dbpath=sandbox/(mode+'.db')
                for suffix in ['', '-wal','-shm']:
                    p=Path(str(dbpath)+suffix)
                    if p.exists():out(p).unlink()
                cli(b,dbpath,data/(mode+'.txt'),sandbox/(mode+'.stdout'))
                db=verified_db(b,dbpath)
                try:
                    if mode.startswith('import'):
                        raw=canonical(db,'SELECT * FROM stage ORDER BY CAST(id AS INTEGER);',sandbox/(mode+'-raw.txt'))
                        equal_file(raw['path'] if Path(raw['path']).is_absolute() else ROOT/raw['path'],data/'raw-expected.txt')
                    if mode!='import':
                        result=canonical(db,read(DATA/'select.txt'),sandbox/(mode+'-typed.txt'));equal_file(ROOT/result['path'],data/'expected.txt')
                        cli_own_result(b['cli'],dbpath,sandbox/(mode+'-own-select.txt'));equal_file(sandbox/(mode+'-own-select.txt'),data/'expected.txt')
                    checks.append({'maximum':maximum,'case':mode,'status':'PASS','rows':32})
                finally:db.close()
            db=verified_db(b,sandbox/'api.db')
            try:
                db.exec('DROP TABLE IF EXISTS bench; DROP TABLE IF EXISTS constrained;');configured(db);db.exec(read(DATA/'schema.txt'))
                ingest_api(db,data,None,'api-prepared-one-txn')
                result=canonical(db,read(DATA/'select.txt'),sandbox/'api-typed.txt');equal_file(ROOT/result['path'],data/'expected.txt')
                # Correct error stage survives sqlite3_finalize repeating the error.
                for sql in ["INSERT INTO bench(id) VALUES(1);",'CREATE TABLE constrained(x CHECK(x>0)); INSERT INTO constrained VALUES(-1);']:
                    try:db.exec(sql)
                    except SqlError as e:
                        if e.stage!='step' or e.code!=19 or not getattr(e,'cleanup_finalize_rc',0):raise
                        checks.append({'maximum':maximum,'case':'negative-constraint','status':'PASS','primary':e.code,'extended':e.extended,'stage':e.stage,'cleanup_finalize_rc':e.cleanup_finalize_rc})
                    else:raise ValueError('negative constraint not rejected')
                stmt=db.prepare('SELECT ?1;')
                try:
                    try:stmt.bind([1,2])
                    except ValueError:pass
                    else:raise ValueError('bad bind arity accepted')
                finally:stmt.close()
                for requested,effective in [(128,128),(64,64),(128,128),(maximum+100,maximum)]:
                    old=db.lib.sqlite3_limit(db.db,2,-1);actual=db.lib.sqlite3_limit(db.db,2,requested)
                    if old!=actual or db.lib.sqlite3_limit(db.db,2,-1)!=effective:raise ValueError('column limit API mismatch')
                checks.append({'maximum':maximum,'case':'API-small/limit/arity','status':'PASS','rows':32})
            finally:db.close()
            # One actual wide row per build is a small API shape/typed preflight;
            # full 3000-row correctness remains stage 2.
            wide=ROOT/m['datasets'][f'wide-{maximum}']['folder'];db=verified_db(b,sandbox/'wide.db')
            try:
                db.exec('DROP TABLE IF EXISTS wide;');db.exec(read(wide/'schema.txt'))
                with (wide/'rows.csv').open(newline='') as f:reader=csv.reader(f);next(reader);row=[token_value(x) for x in next(reader)]
                stmt=db.prepare(read(wide/'insert-template.txt'))
                try:stmt.bind(row);stmt.run()
                finally:stmt.close()
                actual=canonical(db,read(wide/'select.txt'),sandbox/'wide-one-row.txt')
                with (wide/'expected.txt').open('rb') as f:expected=f.readline()
                if (ROOT/actual['path']).read_bytes()!=expected:raise ValueError('wide preflight type/content mismatch')
                checks.append({'maximum':maximum,'case':'wide-one-row','status':'PASS','fields':maximum})
            finally:db.close()
        # Frozen upper/detail UUID fixture and two small nondeterministic LIVE
        # families. No perf_counter or latency sampling on this path.
        from live import smoke
        b=verified(2000)
        for seed in m['config']['seeds']:
            data=ROOT/m['datasets'][f'uuid7-{seed}-smoke']['folder'];path=ART/f'preflight/uuid7-{seed}.db'
            for suffix in ['', '-wal','-shm']:
                candidate=Path(str(path)+suffix)
                if candidate.exists():out(candidate).unlink()
            db=verified_db(b,path)
            try:
                db.exec('PRAGMA foreign_keys=ON;');db.exec(read(data/'schema.txt'));db.exec(read(data/'parents-insert.txt'));db.exec(read(data/'details-insert.txt'))
                if list(db.execute(read(data/'foreign-keys-readback.txt')))!=[[1]]:raise ValueError('frozen UUID FK not enabled')
                for negative in ['negative-orphan.txt','negative-duplicate.txt']:
                    try:db.exec(read(data/negative))
                    except SqlError as e:
                        if e.code!=19 or e.stage!='step' or (negative=='negative-orphan.txt' and e.extended!=787):raise
                        checks.append({'case':'frozen-uuid7-'+negative,'status':'PASS','primary':e.code,'extended':e.extended,'stage':e.stage})
                    else:raise ValueError('frozen UUID negative accepted')
                for stem,expected in [('parents','parents'),('details','details'),('join','join'),('aggregate','aggregate')]:
                    result=canonical(db,read(data/(stem+'-query.txt')),ART/f'preflight/uuid7-{seed}-{stem}.txt');equal_file(ROOT/result['path'],data/(expected+'-expected.txt'))
                indexed=list(db.execute(read(data/'join-query.txt')));plain=list(db.execute(read(data/'join-noindex.txt')))
                if indexed!=plain or list(db.execute('PRAGMA foreign_key_check;')):raise ValueError('frozen UUID join/FK invariant')
                checks.append({'case':'frozen-uuid7','seed':seed,'parents':8,'details':24,'status':'PASS'})
            finally:db.close()
        for mode in m['config']['live']['cases']:
            import shutil
            live_folder=ART/'preflight/live'/mode
            if live_folder.exists():shutil.rmtree(out(live_folder))
            checks.append(smoke(b['library'],m['config']['live'],mode,live_folder))
        # Exercise every ingestion backend on the immutable 32-row smoke fixture.
        import copy
        from unittest.mock import patch
        from compare_cli import exercise
        from benchmark import dispatch
        tiny=copy.deepcopy(m);seed=m['config']['seeds'][0]
        for label in ['full','single']:tiny['datasets'][f'{seed}-{label}']=copy.deepcopy(m['datasets'][f'{seed}-preflight'])
        backends=ART/'preflight/backends'
        if backends.exists():__import__('shutil').rmtree(out(backends))
        backends.mkdir()
        with patch('time.perf_counter_ns',side_effect=AssertionError('untimed preflight sampled duration')):
            for case in m['config']['timed_cases']:
                result=one_case(tiny,case,seed,backends/case,timed=False)
                if result['ns'] is not None or result['row_latency_ns'] or result['batch_latency_ns']:raise ValueError('preflight timing leaked')
                checks.append({'case':'backend-smoke-'+case,'status':'PASS','rows':32,'timing':'NOT_RUN'})
            comparison=ART/'preflight/cli-selfcheck'
            if comparison.exists():__import__('shutil').rmtree(out(comparison))
            # Even accidental C Database use in candidate path must fail.
            with patch('native.Database.__init__',side_effect=AssertionError('candidate path loaded C API')):
                result=exercise(b['cli'],m,comparison,smoke=True)
            if result['status']!='PASS_SCOPED':raise ValueError('CLI comparator smoke failed: '+str(result))
            checks.append({'case':'candidate-own-CLI-selfcheck-with-C-binary','status':'PASS','cases':len(result['cases']),'timing':'NOT_RUN','native_C_API_forbidden':True})
            from live import run as live_run
            for mode in m['config']['live']['cases']:
                path=ART/'preflight/live-batch'/mode
                if path.exists():__import__('shutil').rmtree(out(path))
                checks.append(live_run(b['library'],m['config']['live'],mode,path,commit_mode='batch',smoke_only=True,timed=False,batch_rows=m['config']['batch_rows']))
        path=ART/'preflight/uuid-worker'
        if path.exists():__import__('shutil').rmtree(out(path))
        worker_result=dispatch({'mode':'untimed','family':'uuid-query','case':'join-index','seed':seed,'session':0,'repeat':0,'smoke':True,'build':b,'output':str(path),'cpu':m['config']['cpu']},timeout=60)
        if worker_result['ns'] is not None or worker_result['rows']!=m['config']['frozen_uuid7']['smoke_details']:raise ValueError('untimed worker mismatch')
        checks.append({'case':'bounded-UUID-CLI-worker-smoke','status':'PASS','rows':worker_result['rows'],'timing':'NOT_RUN'})
        report={'status':'PASS','scope':'small pinned C preflight only' ,'performance_status':'NOT_RUN',
                'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'build_sha256':digest(ART/'build.json'),
                'sources':sources(),'checks':checks,'full_wide_3000':'NOT_RUN','catalog_full':'NOT_RUN','Rust':'NOT_RUN'}
        save(output,report);print('PASS: small C native API/CLI preflight; no timing/calibration; full catalog/wide still NOT_RUN')
    except BaseException as e:save(output,{'status':'FAIL','performance_status':'NOT_RUN','error':str(e)});raise


def compatible_family(reference,candidate):return reference==candidate

def cli_own_result(binary,dbpath,destination,*,script=None,fields=7):
    from wireio import normalize_file
    script=script or DATA/'cli-verify.txt'
    result=command([binary,str(dbpath)],input=Path(script).read_bytes())
    if result.stderr:raise ValueError('CLI verification feature/error: '+result.stderr.decode(errors='replace'))
    raw=out(Path(str(destination)+'.stdout'));raw.write_bytes(result.stdout)
    normalized=normalize_file(raw,destination,fields)
    return normalized['rows']


def persistent_cli(binary,dbpath,data,mode,*,timed):
    import queue,threading
    proc=subprocess.Popen([str(binary),str(dbpath)],cwd=ROOT,env=environment(),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
    from support import record_process
    record_process([binary,dbpath],proc.pid)
    q=queue.Queue();errors=[]
    def pump():
        for line in proc.stdout:q.put(line)
        q.put(None)
    def err():errors.append(proc.stderr.read())
    reader=threading.Thread(target=pump,daemon=True);reader.start();er=threading.Thread(target=err,daemon=True);er.start()
    def until(marker):
        got=[]
        while True:
            row=q.get(timeout=600)
            if row is None:raise ValueError('CLI ended before completion ACK')
            if row.strip()==marker:return got
            got.append(row)
    try:
        proc.stdin.write((data/'persistent-start.txt').read_bytes());proc.stdin.flush();preflight=until(b'SQLTEST_READY')
        if [x.strip() for x in preflight][-3:]!=[b'wal',b'1',b'0']:raise ValueError('persistent per-connection readback')
        payload=(data/f'persistent-{mode}.txt').read_bytes()
        start=time.perf_counter_ns() if timed else None
        proc.stdin.write(payload);proc.stdin.flush();rows=until(b'SQLTEST_DONE')
        duration=time.perf_counter_ns()-start if timed else None
        proc.stdin.write((DATA/'quit.txt').read_bytes());proc.stdin.flush();proc.stdin.close()
        proc.wait(timeout=600);reader.join(timeout=5);er.join(timeout=5)
        record_process([binary,dbpath],proc.pid,proc.returncode)
        if proc.returncode or any(errors):raise ValueError('persistent CLI error: '+repr(errors))
        return duration,rows
    finally:
        if proc.poll() is None:proc.kill();proc.wait()

def one_case(m,case,seed,sandbox,*,timed):
    b=verified(2000);label='single' if case.endswith('auto') else 'full';data=folder(m,seed,label)
    out(sandbox/'placeholder');sandbox.mkdir(exist_ok=True);dbpath=sandbox/'case.db'
    if dbpath.exists():raise ValueError('fresh DB required')
    if case.startswith('api'):
        db=verified_db(b,dbpath)
        try:
            configured(db);db.exec(read(DATA/'schema.txt'))
            info=ingest_api(db,data,None,case,timed=timed,batch_rows=m['config']['batch_rows'])
            result=canonical(db,read(DATA/'select.txt'),sandbox/'canonical.txt')
        finally:db.close()
    else:
        mode='import' if case=='cli-import' else 'read-'+case.split('-')[-1]
        if case.startswith('cli-persistent'):
            duration,acks=persistent_cli(b['cli'],dbpath,data,case.split('-')[-1],timed=timed)
            if not acks or acks[-1].strip()!=str(m['datasets'][f'{seed}-'+('single' if case.endswith('auto') else 'full')]['rows']).encode():raise ValueError('persistent row count ACK mismatch')
        else:
            start=time.perf_counter_ns() if timed else None
            response=cli(b,dbpath,data/(mode+'.txt'),sandbox/'stdout')
            duration=time.perf_counter_ns()-start if timed else None
            if response.stdout.splitlines()[-3:]!=[b'wal',b'1',b'0']:raise ValueError('CLI per-connection readback')
        db=verified_db(b,dbpath)
        try:
            if case=='cli-import':
                raw=canonical(db,'SELECT * FROM stage ORDER BY CAST(id AS INTEGER);',sandbox/'raw.txt');equal_file(ROOT/raw['path'],data/'raw-expected.txt');raw.update({'columns':11,'expected':str((data/'raw-expected.txt').relative_to(ROOT))})
                db.exec(read(DATA/'schema.txt'));db.exec(read(DATA/'transform.txt'))
            result=canonical(db,read(DATA/'select.txt'),sandbox/'canonical.txt')
        finally:db.close()
        info={'ns':duration,'rows':result['rows'],'row_latency_ns':[],'batch_latency_ns':[]}
    equal_file(ROOT/result['path'],data/'expected.txt')
    result.update({'columns':7,'expected':str((data/'expected.txt').relative_to(ROOT))})
    if case=='cli-import':info['raw_csv']=raw;info['typed_transform_scope']='outside CLI timer; staging import only is timed'
    if result['rows']!=m['datasets'][f'{seed}-{label}']['rows'] or result['fields']!=result['rows']*7:raise ValueError('ingestion shape incomplete')
    return {**info,'case':case,'seed':seed,'canonical':result,'metric':'Python+native loop/commit' if case.startswith('api') else 'CLI process/I/O/import' if not case.startswith('cli-persistent') else 'persistent CLI pipe/SQL/ACK','status':'PASS'}

def main():
    p=argparse.ArgumentParser();p.add_argument('action',choices=['preflight','measure','compare'])
    p.add_argument('--reviewed-commit');p.add_argument('--output',type=Path);p.add_argument('--engine',choices=['sqlite','sqliters'],default='sqlite');p.add_argument('--binary',type=Path);p.add_argument('--baseline',type=Path)
    a=p.parse_args()
    if a.action=='preflight':
        if a.engine!='sqlite':p.error('preflight is pinned C only; Rust execution forbidden')
        preflight()
    elif a.action=='measure':
        if a.engine!='sqlite':p.error('API/CLI families cannot be substituted; C-only stage 2')
        if not a.reviewed_commit or not a.output:p.error('reviewed commit and fresh output required')
        from benchmark import run
        run(a.reviewed_commit,a.output)
    else:
        if not a.binary or not a.output:p.error('binary and output required')
        if a.engine!='sqliters':p.error('future candidate comparison uses --engine sqliters; no C rerun')
        if not a.baseline:p.error('immutable actual C baseline/golden bundle required; no regeneration')
        from candidate_benchmark import compare
        compare(a.binary,a.output,a.baseline)

if __name__=='__main__':main()
