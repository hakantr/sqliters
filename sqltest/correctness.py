"""Untimed full C correctness catalog. Workers are bounded by parent resource limits."""
import csv
import json
from pathlib import Path
from support import ROOT,ART,out,digest,save
from native import Database,SqlError,wire,TextBytes
from freeze_inputs import DATA,token_value


def typed(db,sql,path):
    import hashlib
    h=hashlib.sha256();rows=fields=0
    with out(path).open('wb') as f:
        for row in db.execute(sql):
            b=wire(row);f.write(b);h.update(b);rows+=1;fields+=len(row)
    return {'path':str(path.relative_to(ROOT)),'rows':rows,'fields':fields,'sha256':h.hexdigest()}


def equal(path,expected):
    if digest(path)!=digest(expected):raise ValueError('full typed content mismatch')


def error_info(db,e):return {'primary':e.code,'extended':e.extended,'stage':e.stage,'offset':e.offset,'message':e.message,'cleanup_finalize_rc':getattr(e,'cleanup_finalize_rc',None),'autocommit':bool(db.lib.sqlite3_get_autocommit(db.db))}


def load_wide(db,data):
    db.exec((data/'schema.txt').read_text());db.exec((DATA/'begin.txt').read_text());s=db.prepare((data/'insert-template.txt').read_text())
    try:
        with (data/'rows.csv').open(newline='') as f:
            reader=csv.reader(f);next(reader)
            for row in reader:s.bind([token_value(x) for x in row]);s.run();s.reset()
    finally:s.close()
    db.exec((DATA/'commit.txt').read_text())


def load_uuid(db,data):
    db.exec((data/'schema.txt').read_text())
    if list(db.execute((data/'foreign-keys-readback.txt').read_text()))!=[[1]]:raise ValueError('FK not enabled')
    db.exec((data/'parents-insert.txt').read_text());db.exec((data/'details-insert.txt').read_text())


def uuid_check(db,data,output,parents,details):
    artifacts=[]
    for stem,count,fields in [('parents',parents,3),('details',details,4),('join',details,4),('aggregate',parents,3)]:
        item=typed(db,(data/(stem+'-query.txt')).read_text(),output/(stem+'.wire'));equal(ROOT/item['path'],data/(stem+'-expected.txt'))
        if item['rows']!=count or item['fields']!=count*fields:raise ValueError('UUID incomplete fields/rows')
        item['expected']=str((data/(stem+'-expected.txt')).relative_to(ROOT));item['columns']=fields;artifacts.append(item)
    plain=typed(db,(data/'join-noindex.txt').read_text(),output/'join-noindex.wire');equal(ROOT/plain['path'],data/'join-expected.txt');plain.update({'columns':4,'expected':str((data/'join-expected.txt').relative_to(ROOT))});artifacts.append(plain)
    errors=[]
    for leaf,ext in [('negative-orphan.txt',787),('negative-duplicate.txt',1555)]:
        try:db.exec((data/leaf).read_text())
        except SqlError as e:
            if e.code!=19 or e.extended!=ext or e.stage!='step':raise
            errors.append(error_info(db,e))
        else:raise ValueError('UUID negative not rejected')
    if list(db.execute('SELECT count(*) FROM uuid_parent; SELECT count(*) FROM uuid_detail;'))!=[[parents],[details]] or list(db.execute('PRAGMA foreign_key_check;')):raise ValueError('UUID negative changed table/FK')
    return artifacts,errors


def plan(m):
    jobs=[]
    for w in m['config']['wide_columns']:jobs.append({'kind':'wide','id':f'wide-{w}','maximum':w})
    jobs.append({'kind':'append','id':'append-all-seeds','maximum':2000})
    for seed in m['config']['seeds']:jobs.append({'kind':'uuid-full','id':f'uuid-{seed}','seed':seed,'maximum':2000})
    for c in m['catalog_scenarios']:jobs.append({'kind':'core','id':c['id'],'spec':c,'maximum':2000})
    for c in m['catalog']:
        if c.get('kind')=='boundary_sql':jobs.append({'kind':'boundary','id':f"max{c['future_max_column']}-{Path(c['source_path']).stem}",'spec':c,'maximum':c['future_max_column']})
    for c in m['catalog_additional_cases']:
        if c['family'] in ['optional','conflict','ddl_fk','generated','storage','stress'] and not c['id'].startswith('wide-'):
            jobs.append({'kind':'additional','id':c['id'],'spec':c,'maximum':c.get('future_max_column',2000)})
    return jobs


def run(m,spec,build,output):
    output=out(output);output.mkdir(exist_ok=True);path=output/'case.db';db=Database(build['library'],path);artifacts=[];errors=[];status='PASS';rows=None
    def execute_record(sql):
        import hashlib
        h=hashlib.sha256();n=fcount=0;dest=output/f'output-{len(artifacts)}.variable-wire'
        try:
            with out(dest).open('wb') as f:
                for row in db.execute(sql):
                    b=str(len(row)).encode()+b'\t'+wire(row);f.write(b);h.update(b);n+=1;fcount+=len(row)
        finally:
            artifacts.append({'path':str(dest.relative_to(ROOT)),'rows':n,'fields':fcount,'sha256':digest(dest),'format':'variable-wire'})
    def source(name):return (DATA/'catalog'/name).read_text()
    try:
        kind=spec['kind']
        if kind=='wide':
            w=spec['maximum'];data=ROOT/m['datasets'][f'wide-{w}']['folder'];load_wide(db,data)
            item=typed(db,(data/'select.txt').read_text(),output/'base.wire');equal(ROOT/item['path'],data/'expected.txt')
            if item['rows']!=m['config']['wide_rows'] or item['fields']!=w*m['config']['wide_rows']:raise ValueError('wide field coverage')
            item.update({'columns':w,'expected':str((data/'expected.txt').relative_to(ROOT))});artifacts.append(item)
            execute_record((data/'workload.txt').read_text())
            item=typed(db,(data/'select.txt').read_text(),output/'mutated.wire');equal(ROOT/item['path'],data/'mutated-expected.txt');item.update({'columns':w,'expected':str((data/'mutated-expected.txt').relative_to(ROOT))});artifacts.append(item);rows=item['rows']
        elif kind=='append':
            db.exec((DATA/'schema.txt').read_text());expected=b''
            for label,seeds in [('full',m['config']['seeds'][:1]),('append',m['config']['seeds'])]:
                for seed in seeds:
                    data=ROOT/m['datasets'][f'{seed}-{label}']['folder'];db.exec((data/'insert-one-txn.txt').read_text());expected+=(data/'expected.txt').read_bytes()
            item=typed(db,(DATA/'select.txt').read_text(),output/'append.wire')
            if (ROOT/item['path']).read_bytes()!=expected or item['rows']!=m['config']['normal_rows']+3*m['config']['append_rows']:raise ValueError('append content/id ranges')
            item['columns']=7;item['expected_parts']=[str((ROOT/m['datasets'][f'{m["config"]["seeds"][0]}-full']['folder']/'expected.txt').relative_to(ROOT))]+[str((ROOT/m['datasets'][f'{seed}-append']['folder']/'expected.txt').relative_to(ROOT)) for seed in m['config']['seeds']];artifacts.append(item);rows=item['rows']
        elif kind=='uuid-full':
            data=ROOT/m['datasets'][f'uuid7-{spec["seed"]}-full']['folder'];load_uuid(db,data);artifacts,errors=uuid_check(db,data,output,m['config']['frozen_uuid7']['parents'],m['config']['frozen_uuid7']['details'])
        elif kind in ['core','boundary']:
            c=spec['spec'];negative=c.get('intentionally_negative',False) if kind=='core' else c['limit_delta']>0
            if kind=='core':
                for pre in c['prerequisites']:db.exec(source(pre))
                sql=source(c['sql_path'])
            else:sql=source(c['source_path'])
            try:execute_record(sql)
            except SqlError as e:
                known={'negative-syntax':(1,None,'prepare'),'negative-unknown-column':(1,None,'prepare'),'negative-unique':(19,1555,'step'),'negative-check':(19,275,'step'),'negative-not-null':(19,1299,'step'),'negative-fk':(19,787,'step'),'negative-strict-type':(19,3091,'step')}
                wanted=(1,None,'prepare') if kind=='boundary' else known.get(c['id'])
                if not negative or wanted is None or e.code!=wanted[0] or (wanted[1] is not None and e.extended!=wanted[1]) or e.stage!=wanted[2]:raise
                errors.append(error_info(db,e));status='EXPECTED_ERROR'
            else:
                if negative:raise ValueError('expected error accepted')
        else:
            c=spec['spec'];family=c['family']
            if c['id'].startswith('wide-'):
                status='NOT_RUN';return {'status':status,'id':spec['id'],'reason':'covered by full wide+mutation typed worker','artifacts':[],'errors':[]}
            if family=='conflict':
                db.exec(source(c['paths'][0]))
                try:db.exec(source(c['paths'][1]))
                except SqlError as e:
                    if e.code!=19 or e.extended!=2067 or e.stage!='step':raise
                    errors.append(error_info(db,e))
                else:raise ValueError('conflict action accepted')
                got=[row[0] for row in db.execute('SELECT id FROM conflict_demo ORDER BY id;')]
                if got!=c['expected_ids'] or bool(db.lib.sqlite3_get_autocommit(db.db))!=c['expected_autocommit_after_error']:raise ValueError('FAIL/ABORT/ROLLBACK effect mismatch')
                execute_record(source(c['paths'][2]));status='EXPECTED_ERROR'
            elif family=='storage':
                db.exec(source(c['paths'][0]));db.close();db=Database(build['library'],path)
                if list(db.execute('PRAGMA page_size;'))!=[[c['expected_page_size']]]:raise ValueError('page size/reopen')
                execute_record(source(c['paths'][1]))
                if list(db.execute('SELECT count(*) FROM page_probe;'))!=[[c['planned_records']]]:raise ValueError('reopened rows')
            else:
                for pre in c['prerequisites']:db.exec(source(pre))
                for name in c['paths']:execute_record(source(name))
                if c['id']=='relational-mutation-stress':
                    actual=sum(next(db.execute(f'SELECT count(*) FROM {name};'))[0] for name in ['customers','orders','items','employees','measurements'])
                    if actual!=c['planned_records_after']:raise ValueError('relational mutation row count')
        integrity=list(db.execute('PRAGMA integrity_check;'))
        if integrity!=[[TextBytes(b'ok')]]:raise ValueError('integrity_check failed')
        return {'status':status,'id':spec['id'],'kind':kind,'rows':rows,'artifacts':artifacts,'errors':errors,'integrity':'ok','autocommit_end':bool(db.lib.sqlite3_get_autocommit(db.db))}
    finally:db.close()
