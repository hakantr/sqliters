#!/usr/bin/env python3
"""Prepare every benchmark input physically before timing; no SQLite calls."""
import csv
import hashlib
import io
import json
from pathlib import Path
import random
import struct
import sys
sys.dont_write_bytecode=True
from support import ROOT,ART,out,digest,save,sources
from native import wire

DATA=ART/'inputs'
SCHEMA='CREATE TABLE bench(id INTEGER PRIMARY KEY,g INTEGER,n INTEGER,r REAL,name TEXT,note TEXT,b BLOB);\n'
STAGE='CREATE TABLE stage(id TEXT,g TEXT,g_null TEXT,n TEXT,n_null TEXT,r TEXT,name TEXT,name_null TEXT,note TEXT,note_null TEXT,b_hex TEXT);\n'
TRANSFORM="INSERT INTO bench SELECT CAST(id AS INTEGER),CASE g_null WHEN '1' THEN NULL ELSE CAST(g AS INTEGER) END,CASE n_null WHEN '1' THEN NULL ELSE CAST(n AS INTEGER) END,CAST(r AS REAL),CASE name_null WHEN '1' THEN NULL ELSE name END,CASE note_null WHEN '1' THEN NULL ELSE note END,unhex(b_hex) FROM stage;\n"
SETTINGS='PRAGMA journal_mode=WAL;\nPRAGMA synchronous=NORMAL;\nPRAGMA wal_autocheckpoint=0;\n'
READBACK='PRAGMA journal_mode;\nPRAGMA synchronous;\nPRAGMA wal_autocheckpoint;\n'
QUERY='SELECT id,g,n,r,name,note,b FROM bench ORDER BY id;\n'
HEADER=['id','g','g_null','n','n_null','r','name','name_null','note','note_null','b_hex']

class Random64:
    def __init__(self,seed):self.x=seed
    def next(self):
        self.x=(self.x+0x9e3779b97f4a7c15)&((1<<64)-1);z=self.x
        z=((z^(z>>30))*0xbf58476d1ce4e5b9)&((1<<64)-1);z=((z^(z>>27))*0x94d049bb133111eb)&((1<<64)-1)
        return z^(z>>31)

def literal(v):
    if v is None:return 'NULL'
    if isinstance(v,str):return "CAST(X'"+v.encode().hex()+"' AS TEXT)"
    if isinstance(v,bytes):return "X'"+v.hex()+"'"
    return repr(v)

def values(row):
    return [int(row[0]),None if row[2]=='1' else int(row[1]),None if row[4]=='1' else int(row[3]),float(row[5]),None if row[7]=='1' else row[6],None if row[9]=='1' else row[8],bytes.fromhex(row[10])]

def token_value(s):
    if s=='N':return None
    tag,v=s.split(':',1)
    if tag=='I':return int(v)
    if tag=='R':return struct.unpack('>d',bytes.fromhex(v))[0]
    if tag=='T':return bytes.fromhex(v).decode('utf-8')
    if tag=='B':return bytes.fromhex(v)
    raise ValueError('bad frozen wide token')

def prepare():
    cfg=json.loads((ROOT/'external-config.json').read_text());files=[];datasets={}
    save(ART/'freeze-state.json',{'status':'RUNNING'})
    def add(path,kind,**meta):
        files.append({'path':str(path.relative_to(ROOT)),'kind':kind,'bytes':path.stat().st_size,'sha256':digest(path),**meta})
    def write(path,text,kind='sql_template',**meta):
        out(path).write_text(text);add(path,kind,**meta)
    def rel(path):return json.dumps(str(path.relative_to(ROOT)))
    def dotread(path):return '.read '+rel(path)+'\n'
    try:
        producer_sources=sources()
        write(DATA/'settings.txt',SETTINGS);write(DATA/'readback.txt',READBACK)
        write(DATA/'schema.txt',SCHEMA);write(DATA/'stage-schema.txt',STAGE);write(DATA/'transform.txt',TRANSFORM)
        rawfields=["'T:'||lower(hex("+name+"))" for name in HEADER]
        write(DATA/'cli-raw-verify.txt','.bail on\n.mode list\n.headers off\nSELECT '+"||char(9)||".join(rawfields)+' FROM stage ORDER BY CAST(id AS INTEGER);\n','CLI_raw_CSV_own_select')
        write(DATA/'cli-transform-only.txt',dotread(DATA/'schema.txt')+dotread(DATA/'transform.txt'),'CLI_typed_transform')
        write(DATA/'insert-template.txt','INSERT INTO bench VALUES('+','.join('?' for _ in range(7))+');\n')
        write(DATA/'select.txt',QUERY);write(DATA/'quit.txt','.quit\n')
        cli_fields=["'I:'||CAST(id AS TEXT)","CASE WHEN g IS NULL THEN 'N' ELSE 'I:'||CAST(g AS TEXT) END","CASE WHEN n IS NULL THEN 'N' ELSE 'I:'||CAST(n AS TEXT) END","'RDEC:'||printf('%!.26g',r)","CASE WHEN name IS NULL THEN 'N' ELSE 'T:'||lower(hex(name)) END","CASE WHEN note IS NULL THEN 'N' ELSE 'T:'||lower(hex(note)) END","'B:'||lower(hex(b))"]
        write(DATA/'cli-verify.txt','.bail on\n.mode list\n.headers off\nSELECT '+"||char(9)||".join(cli_fields)+' FROM bench ORDER BY id;\n','CLI_own_typed_select')
        write(DATA/'begin.txt','BEGIN;\n');write(DATA/'commit.txt','COMMIT;\n')
        for seed in cfg['seeds']:
            for label,count,offset in [('full',cfg['normal_rows'],0),('single',cfg['single_rows'],0),('append',cfg['append_rows'],cfg['normal_rows']+cfg['seeds'].index(seed)*cfg['append_rows']),('preflight',32,0)]:
                folder=DATA/f'seed-{seed}/{label}';out(folder/'placeholder')
                rng=Random64(seed);rows=[];sql=[];expected=[];raw=[]
                # Offset append IDs, while values retain the same seeded pattern.
                for i in range(1,count+1):
                    x=rng.next();edge=[-(1<<63),(1<<63)-1,-1,0,1,127,128]
                    n=None if i%13==0 else edge[(i//7)%len(edge)] if i%7==0 else (x%2000000001)-1000000000
                    text=['comma,value','a"quote','line\nbreak','CR\r\nLF','',r'\N','İstanbul 東京 Zoë'][i%7]
                    name=None if i%19==0 else text;note=None if i%5==0 else 'seed-'+str(seed)+'-'+format(x,'016x')
                    b=b'' if i%11==0 else x.to_bytes(8,'big')[:i%9]
                    v=[i+offset,None if i%17==0 else x%97,n,((x%2000001)-1000000)/8.0,name,note,b]
                    row=[str(v[0]),'' if v[1] is None else str(v[1]),str(int(v[1] is None)),'' if n is None else str(n),str(int(n is None)),repr(v[3]),name or '',str(int(name is None)),note or '',str(int(note is None)),b.hex()]
                    rows.append(row);expected.append(wire(v));raw.append(wire(row))
                    sql.append('INSERT INTO bench VALUES('+','.join(literal(x) for x in v)+');\n')
                csvpath=folder/'rows.csv'
                with out(csvpath).open('w',newline='') as stream:
                    w=csv.writer(stream,lineterminator='\r\n');w.writerow(HEADER);w.writerows(rows)
                add(csvpath,'standard_csv',rows=count,fields=11,seed=seed,id_range=[offset+1,offset+count])
                out(folder/'expected.txt').write_bytes(b''.join(expected));add(folder/'expected.txt','typed_expected',rows=count,fields=7)
                out(folder/'raw-expected.txt').write_bytes(b''.join(raw));add(folder/'raw-expected.txt','raw_csv_expected',rows=count,fields=11)
                write(folder/'row-statements.txt',''.join(sql),'frozen_row_sql',rows=count)
                for mode in ['auto','batch','one-txn']:
                    chunks=[]
                    if mode=='auto':chunks=sql
                    else:
                        size=cfg['batch_rows'] if mode=='batch' else count
                        for start in range(0,count,size):chunks+=['BEGIN;\n',*sql[start:start+size],'COMMIT;\n']
                    write(folder/f'insert-{mode}.txt',''.join(chunks),'frozen_insert_script',rows=count)
                imported='.bail on\n'+dotread(DATA/'settings.txt')+dotread(DATA/'stage-schema.txt')+'.import --csv --skip 1 '+rel(csvpath)+' stage\n'
                write(folder/'import.txt',imported+dotread(DATA/'readback.txt'),'native_cli_import_driver',rows=count)
                write(folder/'import-typed.txt',imported+dotread(DATA/'schema.txt')+dotread(DATA/'transform.txt')+dotread(DATA/'readback.txt'),'native_cli_import_typed_driver',rows=count)
                for mode in ['auto','batch','one-txn']:
                    write(folder/f'read-{mode}.txt','.bail on\n'+dotread(DATA/'settings.txt')+dotread(DATA/'schema.txt')+dotread(folder/f'insert-{mode}.txt')+dotread(DATA/'readback.txt'),'native_cli_read_driver',rows=count)
                write(folder/'persistent-start.txt','.bail on\n'+dotread(DATA/'settings.txt')+dotread(DATA/'schema.txt')+dotread(DATA/'readback.txt')+'.print SQLTEST_READY\n','frozen_persistent_start')
                for mode in ['auto','batch']:
                    write(folder/f'persistent-{mode}.txt',dotread(folder/f'insert-{mode}.txt')+'SELECT count(*) FROM bench;\n.print SQLTEST_DONE\n','frozen_persistent_commands',rows=count)
                datasets[f'{seed}-{label}']={'folder':str(folder.relative_to(ROOT)),'rows':count,'seed':seed,'id_range':[offset+1,offset+count]}
        # Physical wide values, statements and canonical expected rows: no runtime CTE/random.
        for width in cfg['wide_columns']:
            folder=DATA/f'wide-{width}';out(folder/'placeholder')
            columns=[f'c{i:04}' for i in range(width)]
            write(folder/'schema.txt','CREATE TABLE wide('+','.join(columns)+');\n')
            write(folder/'insert-template.txt','INSERT INTO wide VALUES('+','.join('?' for _ in columns)+');\n')
            write(folder/'select.txt','SELECT '+','.join(columns)+' FROM wide ORDER BY rowid;\n')
            paths={name:folder/name for name in ['rows.csv','expected.txt','row-statements.txt','insert-one-txn.txt','mutated-expected.txt']}
            rng=Random64(cfg['seeds'][0]^width)
            with out(paths['rows.csv']).open('w',newline='') as cs, out(paths['expected.txt']).open('wb') as es, out(paths['row-statements.txt']).open('w') as ss, out(paths['insert-one-txn.txt']).open('w') as ts, out(paths['mutated-expected.txt']).open('wb') as ms:
                writer=csv.writer(cs,lineterminator='\r\n');writer.writerow(columns);ts.write('BEGIN;\n')
                for row_id in range(cfg['wide_rows']):
                    v=[]
                    for i in range(width):
                        x=rng.next();tag=x%7
                        v.append(None if tag==0 else -(1<<63) if tag==1 else (1<<63)-1 if tag==2 else (x%1000000)/8.0 if tag==3 else 'col-'+str(i)+'-東京' if tag==4 else x.to_bytes(8,'big') if tag==5 else int(x%1000000))
                    mutated=v.copy()
                    if (row_id+1)%3==0:mutated[0]=row_id+1;mutated[width//2]='updated-'+format(row_id+1,'08d');mutated[-1]=format(row_id+1,'08x').encode()
                    ms.write(wire(mutated))
                    encoded=wire(v);writer.writerow(encoded.decode().rstrip('\n').split('\t'));es.write(encoded)
                    statement='INSERT INTO wide VALUES('+','.join(literal(x) for x in v)+');\n';ss.write(statement);ts.write(statement)
                ts.write('COMMIT;\n')
            for name,path in paths.items():add(path,'precomputed_wide_'+name,rows=cfg['wide_rows'],fields=width)
            source_workload=ROOT/f'artifacts/generated/full/stress/wide-{width}-workload.sql'
            write(folder/'workload.txt',source_workload.read_text().replace('wide_'+str(width),'wide'),'mapped_wide_mutation_workload')
            datasets[f'wide-{width}']={'folder':str(folder.relative_to(ROOT)),'rows':cfg['wide_rows'],'columns':width}
        from uuid_family import create
        for seed in cfg['seeds']:
            for label,smoke in [('full',False),('smoke',True)]:
                uuid_files,meta=create(seed,cfg['frozen_uuid7'],smoke=smoke)
                uuid_folder=DATA/f'uuid7-{seed}-{label}'
                for query_leaf in ['join-cli-index.txt','join-cli-noindex.txt']:
                    uuid_files[query_leaf]=uuid_files[query_leaf].replace('.headers off\n','.headers off\n'+dotread(DATA/'settings.txt')+dotread(DATA/'readback.txt')+'.print SQLTEST_QUERY_BEGIN\n')
                uuid_files['load-cli.txt']='.bail on\n'+dotread(DATA/'settings.txt')+dotread(uuid_folder/'schema.txt')+dotread(uuid_folder/'parents-insert.txt')+dotread(uuid_folder/'details-insert.txt')+dotread(DATA/'readback.txt')
                for leaf,text in uuid_files.items():write(uuid_folder/leaf,text,'frozen_uuid7_'+leaf,**meta)
                datasets[f'uuid7-{seed}-{label}']={'folder':str(uuid_folder.relative_to(ROOT)),**meta}
        from live import SCHEMA as LIVE_SCHEMA,SQL as LIVE_SQL
        write(DATA/'live/schema.txt',LIVE_SCHEMA,'live_operator_SQL')
        for name,sql in LIVE_SQL.items():write(DATA/f'live/{name}.txt',sql+'\n','live_operator_SQL')
        # Copy existing SQL catalog into the ignored immutable freeze bundle.
        catalog=[];old=json.loads((ROOT/'corpus-manifest.json').read_text())
        for item in old['files']:
            if item['path'].endswith('.sql'):
                src=ROOT/item['path'];dst=DATA/'catalog'/item['path'];out(dst).write_bytes(src.read_bytes());add(dst,'catalog_sql',source_path=item['path']);catalog.append({'path':str(dst.relative_to(ROOT)),'source_path':item['path'],**{k:item[k] for k in item if k not in ['path','sha256','bytes']}})
        order=[];rng=random.Random(cfg['order_seed'])
        for session in range(cfg['sessions']):
            for repeat in range(cfg['repeats']):
                block=[{'seed':seed,'case':case} for seed in cfg['seeds'] for case in cfg['timed_cases']];rng.shuffle(block)
                order.extend({'session':session,'repeat':repeat,**x} for x in block)
        uuid_order=[{'session':s,'repeat':rep,'seed':seed,'case':case} for s in range(cfg['sessions']) for rep in range(cfg['repeats']) for seed in cfg['seeds'] for case in cfg['frozen_uuid7']['cli_query_timing']['cases']]
        rng.shuffle(uuid_order)
        live_order=[{'session':s,'repeat':rep,'case':mode+'-'+policy,'uuid_mode':mode,'commit_mode':policy} for s in range(cfg['sessions']) for rep in range(cfg['repeats']) for mode in cfg['live']['cases'] for policy in cfg['live']['commit_modes']]
        rng.shuffle(live_order)
        write(DATA/'live-order.txt',''.join(json.dumps(x,sort_keys=True)+'\n' for x in live_order),'frozen_live_family_order',records=len(live_order))
        write(DATA/'uuid-query-order.txt',''.join(json.dumps(x,sort_keys=True)+'\n' for x in uuid_order),'frozen_uuid_query_order',records=len(uuid_order))
        write(DATA/'order.txt',''.join(json.dumps(x,sort_keys=True)+'\n' for x in order),'frozen_order',records=len(order))
        manifest={'schema':1,'status':'FROZEN','performance_status':'NOT_RUN','config':cfg,'sources':producer_sources,'producer_python':sys.version,
                  'files':files,'datasets':datasets,'catalog':catalog,'catalog_scenarios':old['scenarios'],'catalog_additional_cases':old.get('additional_cases',[]),'csv':'UTF-8 RFC-style comma/quote/CRLF; null flags explicit; no magic null marker',
                  'timed_generation_forbidden':True,'canonical_wire':'v1: tab-separated N/I:decimal/R:IEEEbighex/T:utf8hex/B:hex; LF row terminator'}
        if sources()!=producer_sources:raise ValueError('producer source changed during freeze')
        save(ROOT/'external-input-manifest.json',manifest);save(ART/'freeze-state.json',{'status':'FROZEN','manifest_sha256':digest(ROOT/'external-input-manifest.json')})
        print('Frozen files:',len(files),'normal datasets:',len(datasets)-len(cfg['wide_columns']),'wide datasets:',len(cfg['wide_columns']),flush=True)
    except BaseException as e:save(ART/'freeze-state.json',{'status':'FAIL','error':str(e)});raise


def verify(manifest=None):
    m=manifest if manifest is not None else json.loads((ROOT/'external-input-manifest.json').read_text())
    cfg=json.loads((ROOT/'external-config.json').read_text())
    state=json.loads((ART/'freeze-state.json').read_text())
    if state.get('status')!='FROZEN' or state.get('manifest_sha256')!=digest(ROOT/'external-input-manifest.json'):raise ValueError('freeze incomplete/failed/stale')
    if m.get('status')!='FROZEN' or m['config']!=cfg or m['sources']!=sources():raise ValueError('freeze config/source drift')
    expected={f'{seed}-{label}' for seed in cfg['seeds'] for label in ['full','single','append','preflight']}|{f'wide-{w}' for w in cfg['wide_columns']}|{f'uuid7-{seed}-{label}' for seed in cfg['seeds'] for label in ['full','smoke']}
    if set(m['datasets'])!=expected or not m['catalog']:raise ValueError('incomplete dataset/catalog matrix')
    if len(m['catalog_scenarios'])!=75 or len({x['id'] for x in m['catalog_scenarios']})!=75 or sum(e.get('kind')=='boundary_sql' for e in m['catalog'])!=72:raise ValueError('incomplete core/boundary catalog')
    paths=[e['path'] for e in m['files']]
    if not paths or len(paths)!=len(set(paths)) or len(paths)!=len({str((ROOT/p).resolve()) for p in paths}):raise ValueError('empty/duplicate frozen files')
    for leaf in ['settings.txt','schema.txt','stage-schema.txt','transform.txt','readback.txt','insert-template.txt','select.txt','begin.txt','commit.txt','quit.txt','cli-verify.txt','cli-raw-verify.txt','cli-transform-only.txt','order.txt','uuid-query-order.txt','live-order.txt','live/schema.txt','live/client-parent.txt','live/client-detail.txt','live/server-parent.txt','live/server-detail.txt']:
        if str((DATA/leaf).relative_to(ROOT)) not in paths:raise ValueError('missing mandatory operator/order file')
    for name,d in m['datasets'].items():
        folder=Path(d['folder'])
        if name.startswith('uuid7-'):
            for leaf in ['parents.csv','details.csv','parents-expected.txt','details-expected.txt','schema.txt','join-query.txt','join-noindex.txt','join-expected.txt','aggregate-query.txt','aggregate-expected.txt','foreign-keys-readback.txt','negative-orphan.txt','negative-duplicate.txt','load-cli.txt','join-cli-index.txt','join-cli-noindex.txt']:
                if str(folder/leaf) not in paths:raise ValueError('missing frozen UUID file')
            key='smoke_parents' if name.endswith('-smoke') else 'parents'
            if d['parents']!=cfg['frozen_uuid7'][key] or d['details']!=cfg['frozen_uuid7']['smoke_details' if name.endswith('-smoke') else 'details']:raise ValueError('UUID row contract mismatch')
            continue
        for leaf in ['rows.csv','expected.txt','row-statements.txt','insert-one-txn.txt']:
            if str(folder/leaf) not in paths:raise ValueError('missing required dataset input')
        if name.startswith('wide-'):
            for leaf in ['workload.txt','mutated-expected.txt']:
                if str(folder/leaf) not in paths:raise ValueError('missing wide mutation input/golden')
        count=cfg['wide_rows'] if name.startswith('wide-') else cfg['normal_rows'] if name.endswith('-full') else cfg['single_rows'] if name.endswith('-single') else cfg['append_rows'] if name.endswith('-append') else 32
        if d['rows']!=count:raise ValueError('dataset row contract mismatch')
    for e in m['files']:
        path=ROOT/e['path']
        if not path.resolve().is_relative_to(DATA) or any(p.is_symlink() for p in [path,*path.parents]) or e['bytes']<=0 or path.stat().st_size!=e['bytes'] or digest(path)!=e['sha256']:raise ValueError('frozen input drift: '+e['path'])
    order=[json.loads(x) for x in (DATA/'order.txt').read_text().splitlines()]
    keys=[(x['session'],x['repeat'],x['seed'],x['case']) for x in order]
    required={(s,r,seed,c) for s in range(cfg['sessions']) for r in range(cfg['repeats']) for seed in cfg['seeds'] for c in cfg['timed_cases']}
    if len(keys)!=len(required) or set(keys)!=required:raise ValueError('frozen order incomplete/duplicate')
    for leaf,family in [('uuid-query-order.txt','uuid'),('live-order.txt','live')]:
        entries=[json.loads(line) for line in (DATA/leaf).read_text().splitlines()]
        keys=[(x['session'],x['repeat'],x.get('seed'),x['case']) for x in entries]
        required={(s,r,seed,c) for s in range(cfg['sessions']) for r in range(cfg['repeats']) for seed in cfg['seeds'] for c in cfg['frozen_uuid7']['cli_query_timing']['cases']} if family=='uuid' else {(s,r,None,mode+'-'+policy) for s in range(cfg['sessions']) for r in range(cfg['repeats']) for mode in cfg['live']['cases'] for policy in cfg['live']['commit_modes']}
        if len(keys)!=len(required) or set(keys)!=required:raise ValueError('UUID/LIVE order matrix')
    return m

if __name__=='__main__':prepare()
