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
    rows=decoded(data/'rows.csv');sql=read(data/'row-statements.txt').splitlines()
    stmt=db.prepare(read(DATA/'insert-template.txt')) if case!='api-exec-auto' else None
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
    return {'rows':len(rows),'ns':total,'row_latency_ns':row_times,'batch_latency_ns':batch_times}

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
            if live_folder.exists():shutil.rmtree(live_folder)
            checks.append(smoke(b['library'],m['config']['live'],mode,live_folder))
        report={'status':'PASS','scope':'small pinned C preflight only' ,'performance_status':'NOT_RUN',
                'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'build_sha256':digest(ART/'build.json'),
                'sources':sources(),'checks':checks,'full_wide_3000':'NOT_RUN','catalog_full':'NOT_RUN','Rust':'NOT_RUN'}
        save(output,report);print('PASS: small C native API/CLI preflight; no timing/calibration; full catalog/wide still NOT_RUN')
    except BaseException as e:save(output,{'status':'FAIL','performance_status':'NOT_RUN','error':str(e)});raise


def review_gate(commit):
    m=verify();p=json.loads((ROOT/'results/external-preflight.json').read_text())
    if p.get('status')!='PASS' or p.get('performance_status')!='NOT_RUN' or p['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json') or p['build_sha256']!=digest(ART/'build.json') or p['sources']!=sources():raise ValueError('preflight stale/missing')
    for name in [*sources(),'external-input-manifest.json']:
        before=command(['git','show',commit+':sqltest/'+name],cwd=ROOT.parent).stdout
        if before!=(ROOT/name).read_bytes():raise ValueError('not reviewed clean code/config/input: '+name)
    for maximum in [128,2000,4096]:verified(maximum)
    return m


def compatible_family(reference,candidate):return reference==candidate

def cli_own_result(binary,dbpath,destination):
    result=command([binary,str(dbpath)],input=(DATA/'cli-verify.txt').read_bytes())
    if result.stderr:raise ValueError('CLI verification feature/error: '+result.stderr.decode(errors='replace'))
    lines=[]
    for raw in result.stdout.splitlines():
        tokens=raw.decode('ascii').split('\t')
        if len(tokens)!=7:raise ValueError('candidate typed wire field count')
        converted=[]
        for token in tokens:
            if token.startswith('RDEC:'):
                import struct
                token='R:'+struct.pack('>d',float(token[5:])).hex()
            if not (token=='N' or token.startswith(('I:','R:','T:','B:'))):raise ValueError('candidate typed wire tag')
            converted.append(token)
        lines.append(('\t'.join(converted)+'\n').encode())
    out(destination).write_bytes(b''.join(lines));return len(lines)

def persistent_cli(binary,dbpath,data,mode,*,timed):
    import queue,threading
    proc=subprocess.Popen([str(binary),str(dbpath)],cwd=ROOT,env=environment(),stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
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
        proc.stdin.write((data/'persistent-start.txt').read_bytes());proc.stdin.flush();until(b'SQLTEST_READY')
        payload=(data/f'persistent-{mode}.txt').read_bytes()
        start=time.perf_counter_ns() if timed else None
        proc.stdin.write(payload);proc.stdin.flush();rows=until(b'SQLTEST_DONE')
        duration=time.perf_counter_ns()-start if timed else None
        proc.stdin.write((DATA/'quit.txt').read_bytes());proc.stdin.flush();proc.stdin.close()
        proc.wait(timeout=600);reader.join(timeout=5);er.join(timeout=5)
        if proc.returncode or any(errors):raise ValueError('persistent CLI error: '+repr(errors))
        return duration,rows
    finally:
        if proc.poll() is None:proc.kill();proc.wait()

def candidate_compare(binary,output):
    m=verify();path=Path(binary).resolve();output=out(Path(output));output.mkdir(exist_ok=False)
    if not path.is_file():raise ValueError('candidate binary required')
    results=[]
    # No native C library is opened anywhere in this candidate path.
    for seed in m['config']['seeds']:
        for mode in ['import-typed','read-auto','read-batch','persistent-auto','persistent-batch']:
            label='single' if mode.endswith('auto') else 'full';data=folder(m,seed,label)
            sandbox=output/f'{seed}-{mode}';sandbox.mkdir();dbpath=sandbox/'candidate.db'
            try:
                if mode.startswith('persistent'):persistent_cli(path,dbpath,data,mode.split('-')[1],timed=False)
                else:cli({'cli':str(path)},dbpath,data/(mode+'.txt'),sandbox/'stdout')
                count=cli_own_result(path,dbpath,sandbox/'canonical.txt');equal_file(sandbox/'canonical.txt',data/'expected.txt')
                results.append({'seed':seed,'case':mode,'status':'PASS','rows':count})
            except subprocess.CalledProcessError as e:results.append({'seed':seed,'case':mode,'status':'NOT_SUPPORTED_OR_ERROR','stderr':e.stderr.decode(errors='replace')})
            except Exception as e:results.append({'seed':seed,'case':mode,'status':'DIFF','error':str(e)})
    save(output/'comparison.json',{'engine':'sqliters','binary_sha256':digest(path),'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'cases':results,'timing':'NOT_RUN','API':'NOT_RUN: native API adapter required','C_loaded':False})

def one_case(m,case,seed,sandbox,*,timed):
    b=verified(2000);data=folder(m,seed,'single' if case.endswith('auto') else 'full')
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
            cli(b,dbpath,data/(mode+'.txt'),sandbox/'stdout')
            duration=time.perf_counter_ns()-start if timed else None
        db=verified_db(b,dbpath)
        try:
            if case=='cli-import':
                raw=canonical(db,'SELECT * FROM stage ORDER BY CAST(id AS INTEGER);',sandbox/'raw.txt');equal_file(ROOT/raw['path'],data/'raw-expected.txt')
                db.exec(read(DATA/'schema.txt'));db.exec(read(DATA/'transform.txt'))
            result=canonical(db,read(DATA/'select.txt'),sandbox/'canonical.txt')
        finally:db.close()
        info={'ns':duration,'rows':result['rows'],'row_latency_ns':[],'batch_latency_ns':[]}
    equal_file(ROOT/result['path'],data/'expected.txt')
    return {**info,'case':case,'seed':seed,'canonical':result,'metric':'Python+native loop/commit' if case.startswith('api') else 'CLI process/I/O/import' if not case.startswith('cli-persistent') else 'persistent CLI pipe/SQL/ACK','status':'PASS'}

def full_wide_and_append(m,output):
    checks=[]
    for width in m['config']['wide_columns']:
        b=verified(width);data=ROOT/m['datasets'][f'wide-{width}']['folder'];db=verified_db(b,output/f'wide-{width}.db')
        try:
            db.exec(read(data/'schema.txt'));db.exec(read(DATA/'begin.txt'));stmt=db.prepare(read(data/'insert-template.txt'))
            try:
                with (data/'rows.csv').open(newline='') as f:
                    reader=csv.reader(f);next(reader)
                    for row in reader:stmt.bind([token_value(x) for x in row]);stmt.run();stmt.reset()
            finally:stmt.close()
            db.exec(read(DATA/'commit.txt'));result=canonical(db,read(data/'select.txt'),output/f'wide-{width}-canonical.txt');equal_file(ROOT/result['path'],data/'expected.txt')
            if result['rows']!=3000 or result['fields']!=3000*width:raise ValueError('wide content incomplete')
            checks.append({'case':f'wide-{width}','status':'PASS','rows':result['rows'],'fields':result['fields']})
        finally:db.close()
    b=verified(2000);db=verified_db(b,output/'append.db')
    try:
        db.exec(read(DATA/'schema.txt'));first=folder(m,m['config']['seeds'][0],'full');db.exec(read(first/'insert-one-txn.txt'))
        expected=(first/'expected.txt').read_bytes()
        for seed in m['config']['seeds']:
            data=folder(m,seed,'append');db.exec(read(data/'insert-one-txn.txt'));expected+=(data/'expected.txt').read_bytes()
        actual=canonical(db,read(DATA/'select.txt'),output/'append-canonical.txt')
        if (ROOT/actual['path']).read_bytes()!=expected:raise ValueError('append unique-id content mismatch')
        checks.append({'case':'append-all-seeds','status':'PASS','rows':actual['rows']})
    finally:db.close()
    return checks

def catalog_correctness(m,output):
    results=[];catalog_root=DATA/'catalog'
    cases=[]
    for row in m['catalog_scenarios']:cases.append((row['id'],2000,[*row['prerequisites'],row['sql_path']],row.get('intentionally_negative',False)))
    for row in m['catalog']:
        if row.get('kind')=='boundary_sql':cases.append((f"max{row['future_max_column']}-{Path(row['source_path']).stem}",row['future_max_column'],[row['source_path']],row['limit_delta']>0))
    # Optional source SQL and additional mutation/page/conflict cases stay
    # explicitly catalogued; phased/API/crash schedules are never concatenated.
    for row in m['catalog_additional_cases']:
        if row['family']=='optional':cases.append((row['id'],2000,row['prerequisites']+row['paths'],False))
    for name,maximum,paths,negative in cases:
        b=verified(maximum);db=verified_db(b,output/(name+'.db'));last=None
        try:
            for path in paths:
                for _ in db.execute(read(catalog_root/path)):pass
            if negative:raise ValueError('expected negative accepted')
            results.append({'case':name,'status':'PASS'})
        except SqlError as e:
            results.append({'case':name,'status':'EXPECTED_ERROR' if negative else 'FAIL','primary':e.code,'extended':e.extended,'stage':e.stage,'message':e.message,'offset':e.offset})
        except Exception as e:results.append({'case':name,'status':'FAIL','error':str(e)})
        finally:db.close()
    for row in m['catalog_additional_cases']:
        if row['family']!='optional':results.append({'case':row['id'],'status':'NOT_RUN','reason':'phased/reopen/mutation/WAL/API schedule requires dedicated backend; not concatenated'})
    return results

def measure(commit,output):
    if not json.loads((ROOT/'external-config.json').read_text()).get('measurement_implementation_ready'):raise RuntimeError('Stage 2A implementation/review/commit required; no timing taken')
    m=review_gate(commit);output=out(Path(output));output.mkdir(exist_ok=False)
    save(output/'status.json',{'status':'RUNNING'})
    try:
        raw=[];deadline=time.monotonic()+m['config']['full_timeout_seconds']
        for seed in m['config']['seeds']:
            for case in m['config']['timed_cases']:one_case(m,case,seed,output/f'warm-{seed}-{case}',timed=False)
        order=[json.loads(line) for line in (DATA/'order.txt').read_text().splitlines()]
        for i,row in enumerate(order):
            if time.monotonic()>deadline:raise TimeoutError('full run deadline')
            sample=one_case(m,row['case'],row['seed'],output/f'sample-{i:04}',timed=True);sample.update(row);raw.append(sample)
            save(output/'raw.json',raw)
        correctness=full_wide_and_append(m,output);catalog=catalog_correctness(m,output/'catalog')
        summary={'status':'PASS' if all(x['status']!='FAIL' for x in catalog) else 'FAIL','samples':len(raw),'correctness':correctness,'catalog':catalog,'Rust':'NOT_RUN','input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'raw_sha256':digest(output/'raw.json'),'sources':sources()}
        save(output/'status.json',summary);save(ROOT/'results/external-summary.json',summary)
    except BaseException as e:save(output/'status.json',{'status':'FAIL','error':str(e)});raise


def main():
    p=argparse.ArgumentParser();p.add_argument('action',choices=['preflight','measure','compare'])
    p.add_argument('--reviewed-commit');p.add_argument('--output',type=Path);p.add_argument('--engine',choices=['sqlite','sqliters'],default='sqlite');p.add_argument('--binary',type=Path)
    a=p.parse_args()
    if a.action=='preflight':
        if a.engine!='sqlite':p.error('preflight is pinned C only; Rust execution forbidden')
        preflight()
    elif a.action=='measure':
        if a.engine!='sqlite':p.error('API/CLI families cannot be substituted; C-only stage 2')
        if not a.reviewed_commit or not a.output:p.error('reviewed commit and fresh output required')
        measure(a.reviewed_commit,a.output)
    else:
        if not a.binary or not a.output:p.error('binary and output required')
        if a.engine!='sqliters':p.error('future candidate comparison uses --engine sqliters; no C rerun')
        candidate_compare(a.binary,a.output)

if __name__=='__main__':main()
