"""Separate nondeterministic LIVE correctness family. No cross-run hash oracle."""
import csv
import json
import secrets
import uuid
import time
from datetime import datetime,timezone,timedelta
from support import ART,out,save
from native import Database,TextBytes

UUID4="lower(hex(randomblob(4)))||'-'||lower(hex(randomblob(2)))||'-4'||substr(lower(hex(randomblob(2))),2)||'-'||substr('89ab',(random()&3)+1,1)||substr(lower(hex(randomblob(2))),2)||'-'||lower(hex(randomblob(6)))"
NOW="strftime('%Y-%m-%dT%H:%M:%fZ','now')"
SCHEMA=f'''PRAGMA foreign_keys=ON;
CREATE TABLE live_parent(id TEXT NOT NULL PRIMARY KEY DEFAULT ({UUID4}),label TEXT,created_at TEXT DEFAULT ({NOW}));
CREATE TABLE live_detail(id TEXT NOT NULL PRIMARY KEY DEFAULT ({UUID4}),parent_id TEXT NOT NULL REFERENCES live_parent(id),cents INTEGER,created_at TEXT DEFAULT ({NOW}));
CREATE INDEX live_detail_parent_time ON live_detail(parent_id,created_at);
'''

def now():return datetime.now(timezone.utc)
def text(v):return bytes(v).decode() if isinstance(v,TextBytes) else v

def validate_uuid(value,version):
    parsed=uuid.UUID(value)
    if str(parsed)!=value or parsed.version!=version or parsed.variant!=uuid.RFC_4122:raise ValueError('UUID canonical/version/variant invariant')

def expected_from_request(table,mode,requested,returned):
    if mode=='client-uuid4-utc':
        if returned!=requested:raise ValueError('LIVE RETURNING changed requested client payload/id/time')
        return requested.copy()
    if table=='parent':
        if len(returned)!=3 or returned[1]!=requested[0]:raise ValueError('LIVE server changed requested label')
        return [returned[0],requested[0],returned[2]]
    if len(returned)!=4 or returned[1:3]!=requested:raise ValueError('LIVE server changed requested parent/cents')
    return [returned[0],requested[0],requested[1],returned[3]]

SQL={
 'client-parent':'INSERT INTO live_parent(id,label,created_at) VALUES(?,?,?) RETURNING id,label,created_at;',
 'server-parent':'INSERT INTO live_parent(label) VALUES(?) RETURNING id,label,created_at;',
 'client-detail':'INSERT INTO live_detail(id,parent_id,cents,created_at) VALUES(?,?,?,?) RETURNING id,parent_id,cents,created_at;',
 'server-detail':'INSERT INTO live_detail(parent_id,cents) VALUES(?,?) RETURNING id,parent_id,cents,created_at;'}
QUERIES={
 'parents':'SELECT id,label,created_at FROM live_parent ORDER BY id;',
 'details':'SELECT id,parent_id,cents,created_at FROM live_detail ORDER BY id;',
 'fk':'PRAGMA foreign_key_check;',
 'indexed':'SELECT d.id,p.id,d.cents FROM live_detail d INDEXED BY live_detail_parent_time JOIN live_parent p ON p.id=d.parent_id ORDER BY d.id;',
 'unindexed':'SELECT d.id,p.id,d.cents FROM live_detail d NOT INDEXED JOIN live_parent p ON p.id=d.parent_id ORDER BY d.id;'}


def readback(db):return {k:[list(map(text,row)) for row in db.execute(q)] for k,q in QUERIES.items()}


def validate_recorded(snapshot,events,start,end,parents,details,tolerance):
    ps,ds=snapshot['parents'],snapshot['details']
    if len(ps)!=parents or len(ds)!=details or len({x[0] for x in ps+ds})!=parents+details:raise ValueError('LIVE count/uniqueness')
    expected_p=[];expected_d=[]
    for e in events:
        expected=expected_from_request(e['table'],e['mode'],e['requested'],e['returned'])
        if expected!=e['expected_row']:raise ValueError('LIVE event expected not request-derived')
        (expected_p if e['table']=='parent' else expected_d).append(expected)
    if ps!=sorted(expected_p) or ds!=sorted(expected_d):raise ValueError('LIVE same-run request/readback mismatch')
    for row in ps+ds:
        validate_uuid(row[0],4);stamp=datetime.fromisoformat(row[-1].replace('Z','+00:00'))
        if not row[-1].endswith('Z') or stamp.utcoffset()!=timedelta(0) or stamp.isoformat(timespec='milliseconds').replace('+00:00','Z')!=row[-1]:raise ValueError('LIVE canonical UTC timestamp')
        if not start-timedelta(seconds=tolerance)<=stamp<=end+timedelta(seconds=tolerance):raise ValueError('LIVE timestamp range')
    parent_ids={p[0] for p in ps}
    if any(d[1] not in parent_ids for d in ds) or snapshot['fk']:raise ValueError('LIVE orphan/FK')
    expected_join=[[d[0],d[1],d[2]] for d in ds]
    if snapshot['indexed']!=snapshot['unindexed'] or snapshot['indexed']!=expected_join:raise ValueError('LIVE indexed/noindex join')
    cents=sum(e['requested'][1] if e['mode']=='db-default-uuid4-utc' else e['requested'][2] for e in events if e['table']=='detail')
    if sum(d[2] for d in ds)!=cents:raise ValueError('LIVE requested cents sum')
    return {'parents':len(ps),'details':len(ds),'UUID_version':4,'oracle':'same-run requests/readback/invariants; no value SHA oracle','status':'PASS'}


def validate(db,events,start,end,parents,details,tolerance):return validate_recorded(readback(db),events,start,end,parents,details,tolerance)


def run(library,cfg,mode,folder,*,commit_mode='autocommit',timed=False,smoke_only=False,batch_rows=1000):
    from freeze_inputs import DATA
    folder=out(folder);folder.mkdir(exist_ok=True);path=folder/'live.db'
    if path.exists():raise ValueError('fresh LIVE DB required')
    if commit_mode not in ['autocommit','batch']:raise ValueError('unknown LIVE policy')
    def tick():return time.perf_counter_ns() if timed else None
    db=Database(library,path);events=[];parent_ids=[];autocommit_rows=[];step_times=[];commits=[];begins=[];batches=[];generation=[]
    nparent=cfg['smoke_parents'] if smoke_only else cfg['parents'];ndetail=cfg['smoke_details'] if smoke_only else cfg['details']
    prepared={}
    try:
        db.exec((DATA/'settings.txt').read_text());db.exec((DATA/'live/schema.txt').read_text())
        if list(db.execute((DATA/'readback.txt').read_text()))!=[[TextBytes(b'wal')],[1],[0]] or list(db.execute('PRAGMA foreign_keys;'))!=[[1]]:raise ValueError('LIVE settings/FK readback')
        prefix='client' if mode=='client-uuid4-utc' else 'server'
        for table in ['parent','detail']:prepared[table]=db.prepare((DATA/f'live/{prefix}-{table}.txt').read_text())
        begin=(DATA/'begin.txt').read_text();commit=(DATA/'commit.txt').read_text();start=now();t0=tick()
        for table,count in [('parent',nparent),('detail',ndetail)]:
            stmt=prepared[table]
            for i in range(count):
                if commit_mode=='batch' and i%batch_rows==0:
                    batch0=tick();b0=tick();db.exec(begin)
                    if timed:begins.append(tick()-b0)
                g0=tick();payload=['live-'+str(i)] if table=='parent' else [secrets.choice(parent_ids),secrets.randbelow(100001)]
                if mode=='client-uuid4-utc':payload=[str(uuid.uuid4()),*payload,now().isoformat(timespec='milliseconds').replace('+00:00','Z')]
                gen=tick()-g0 if timed else None
                d0=tick();stmt.bind(payload);rows=list(stmt.rows());stmt.reset();db_ns=tick()-d0 if timed else None
                if len(rows)!=1:raise ValueError('LIVE RETURNING arity')
                row=list(map(text,rows[0]));expected=expected_from_request(table,mode,payload,row)
                events.append({'table':table,'mode':mode,'requested':payload.copy(),'returned':row,'expected_row':expected,'generation_ns':gen,'db_step_ns':db_ns,'observed_utc':now().isoformat()})
                if table=='parent':parent_ids.append(row[0])
                if timed:
                    generation.append(gen);step_times.append(db_ns)
                    if commit_mode=='autocommit':autocommit_rows.append(db_ns)
                if commit_mode=='batch' and ((i+1)%batch_rows==0 or i+1==count):
                    c0=tick();db.exec(commit)
                    if timed:commits.append(tick()-c0);batches.append(tick()-batch0)
        duration=tick()-t0 if timed else None;end=now()
        snapshot=readback(db);result=validate_recorded(snapshot,events,start,end,nparent,ndetail,cfg['timestamp_tolerance_seconds'])
        out(folder/'events.jsonl').write_text(''.join(json.dumps(e,ensure_ascii=False)+'\n' for e in events));save(folder/'readback.json',snapshot)
        result.update({'mode':mode,'commit_mode':commit_mode,'rows':nparent+ndetail,'ns':duration,'row_latency_ns':autocommit_rows,'row_step_latency_ns':step_times,'batch_latency_ns':batches,'commit_latency_ns':commits,'begin_latency_ns':begins,
          'generation_ns':sum(generation) if timed and mode=='client-uuid4-utc' else None,'payload_generation_ns':sum(generation) if timed else None,'db_write_ns':sum(step_times)+sum(commits)+sum(begins) if timed else None,
          'server_uuid_time_generation':'included in DB step, cannot isolate' if mode=='db-default-uuid4-utc' else 'client generation measured separately',
          'scope':'small correctness smoke; no timing' if not timed else 'LIVE API loop+generation+DB commit e2e; validation outside',
          'start_utc':start.isoformat(),'end_utc':end.isoformat(),'event_log':str((folder/'events.jsonl').relative_to(__import__('support').ROOT)),'readback_path':str((folder/'readback.json').relative_to(__import__('support').ROOT))})
        return result
    finally:
        for stmt in prepared.values():stmt.close(ignore_error=__import__('sys').exc_info()[0] is not None)
        db.close()


def smoke(library,cfg,mode,folder):return run(library,cfg,mode,folder,smoke_only=True)
