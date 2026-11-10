"""Separate nondeterministic LIVE correctness family. No cross-run hash oracle."""
import csv
import json
import secrets
import uuid
from datetime import datetime,timezone,timedelta
from support import ART,out,save
from native import Database,TextBytes

UUID4="lower(hex(randomblob(4)))||'-'||lower(hex(randomblob(2)))||'-4'||substr(lower(hex(randomblob(2))),2)||'-'||substr('89ab',(random()&3)+1,1)||substr(lower(hex(randomblob(2))),2)||'-'||lower(hex(randomblob(6)))"
NOW="strftime('%Y-%m-%dT%H:%M:%fZ','now')"
SCHEMA=f'''PRAGMA foreign_keys=ON;
CREATE TABLE live_parent(id TEXT PRIMARY KEY DEFAULT ({UUID4}),label TEXT,created_at TEXT DEFAULT ({NOW}));
CREATE TABLE live_detail(id TEXT PRIMARY KEY DEFAULT ({UUID4}),parent_id TEXT NOT NULL REFERENCES live_parent(id),cents INTEGER,created_at TEXT DEFAULT ({NOW}));
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

def validate(db,events,start,end,parents,details,tolerance):
    ps=[list(map(text,row)) for row in db.execute('SELECT id,label,created_at FROM live_parent ORDER BY id;')]
    ds=[list(map(text,row)) for row in db.execute('SELECT id,parent_id,cents,created_at FROM live_detail ORDER BY id;')]
    if len(ps)!=parents or len(ds)!=details or len({x[0] for x in ps+ds})!=parents+details:raise ValueError('LIVE count/uniqueness')
    expected_p=sorted([e['expected_row'] for e in events if e['table']=='parent']);expected_d=sorted([e['expected_row'] for e in events if e['table']=='detail'])
    if ps!=expected_p or ds!=expected_d:raise ValueError('LIVE same-run event/content mismatch')
    for row in ps+ds:
        validate_uuid(row[0],4);stamp=datetime.fromisoformat(row[-1].replace('Z','+00:00'))
        if not row[-1].endswith('Z') or stamp.utcoffset()!=timedelta(0) or stamp.isoformat(timespec='milliseconds').replace('+00:00','Z')!=row[-1]:raise ValueError('LIVE canonical UTC timestamp')
        if not start-timedelta(seconds=tolerance)<=stamp<=end+timedelta(seconds=tolerance):raise ValueError('LIVE timestamp range')
    if list(db.execute('PRAGMA foreign_key_check;')):raise ValueError('LIVE FK')
    joined=list(db.execute('SELECT d.id,p.id,d.cents FROM live_detail d INDEXED BY live_detail_parent_time JOIN live_parent p ON p.id=d.parent_id ORDER BY d.id;'))
    unindexed=list(db.execute('SELECT d.id,p.id,d.cents FROM live_detail d NOT INDEXED JOIN live_parent p ON p.id=d.parent_id ORDER BY d.id;'))
    if joined!=unindexed or len(joined)!=details or sum(x[2] for x in joined)!=sum(e['requested'][1] if e['mode']=='db-default-uuid4-utc' else e['requested'][2] for e in events if e['table']=='detail'):raise ValueError('LIVE JOIN/sum/index invariants')
    return {'parents':len(ps),'details':len(ds),'UUID_version':4,'oracle':'same-run events+invariants; no data/result SHA oracle','status':'PASS'}

def smoke(library,cfg,mode,folder):
    folder=out(folder);folder.mkdir(exist_ok=True);path=folder/'live.db'
    if path.exists():raise ValueError('fresh LIVE DB required')
    db=Database(library,path);events=[];start=now();parents=[]
    try:
        db.exec(SCHEMA)
        for table,count in [('parent',cfg['smoke_parents']),('detail',cfg['smoke_details'])]:
            sql=('INSERT INTO live_parent(id,label,created_at) VALUES(?,?,?) RETURNING id,label,created_at;' if mode=='client-uuid4-utc' else 'INSERT INTO live_parent(label) VALUES(?) RETURNING id,label,created_at;') if table=='parent' else ('INSERT INTO live_detail(id,parent_id,cents,created_at) VALUES(?,?,?,?) RETURNING id,parent_id,cents,created_at;' if mode=='client-uuid4-utc' else 'INSERT INTO live_detail(parent_id,cents) VALUES(?,?) RETURNING id,parent_id,cents,created_at;')
            stmt=db.prepare(sql)
            try:
                for i in range(count):
                    payload=['live-'+str(i)] if table=='parent' else [secrets.choice(parents),secrets.randbelow(100001)]
                    if mode=='client-uuid4-utc':payload=[str(uuid.uuid4()),*payload,now().isoformat(timespec='milliseconds').replace('+00:00','Z')]
                    stmt.bind(payload);rows=list(stmt.rows());stmt.reset()
                    if len(rows)!=1:raise ValueError('LIVE returning arity')
                    row=list(map(text,rows[0]));expected=expected_from_request(table,mode,payload,row);events.append({'table':table,'mode':mode,'requested':payload.copy(),'returned':row,'expected_row':expected,'observed_utc':now().isoformat()})
                    if table=='parent':parents.append(row[0])
            finally:stmt.close()
        end=now();result=validate(db,events,start,end,cfg['smoke_parents'],cfg['smoke_details'],cfg['timestamp_tolerance_seconds'])
        out(folder/'events.jsonl').write_text(''.join(json.dumps(e,ensure_ascii=False)+'\n' for e in events))
        result.update({'mode':mode,'scope':'small correctness smoke; no timing','start_utc':start.isoformat(),'end_utc':end.isoformat()});return result
    finally:db.close()
