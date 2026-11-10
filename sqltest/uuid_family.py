"""Offline seeded RFC9562 UUIDv7 upper/detail CSV/TXT fixture generator."""
import csv
import io
import random
import uuid
from datetime import datetime,timezone,timedelta
from native import wire

SCHEMA='''PRAGMA foreign_keys=ON;
CREATE TABLE uuid_parent(id TEXT NOT NULL PRIMARY KEY,label TEXT,created_at TEXT);
CREATE TABLE uuid_detail(id TEXT NOT NULL PRIMARY KEY,parent_id TEXT NOT NULL REFERENCES uuid_parent(id),cents INTEGER,created_at TEXT);
CREATE INDEX detail_parent_time ON uuid_detail(parent_id,created_at);
'''
PQUERY='SELECT id,label,created_at FROM uuid_parent ORDER BY id;\n'
DQUERY='SELECT id,parent_id,cents,created_at FROM uuid_detail ORDER BY id;\n'
JOIN='SELECT d.id,p.id,d.cents,d.created_at FROM uuid_detail d INDEXED BY detail_parent_time JOIN uuid_parent p ON p.id=d.parent_id ORDER BY d.id;\n'
NOINDEX=JOIN.replace(' INDEXED BY detail_parent_time',' NOT INDEXED')
AGG='SELECT p.id,count(d.id),coalesce(sum(d.cents),0) FROM uuid_parent p LEFT JOIN uuid_detail d ON p.id=d.parent_id GROUP BY p.id ORDER BY p.id;\n'

def uuid7(ms,rng):
    if not 0<=ms<(1<<48):raise ValueError('UUIDv7 millisecond range')
    return str(uuid.UUID(int=(ms<<80)|(7<<76)|(rng.getrandbits(12)<<64)|(2<<62)|rng.getrandbits(62)))

def create(seed,cfg,*,smoke=False):
    rng=random.Random(seed);np=cfg['smoke_parents'] if smoke else cfg['parents'];nd=cfg['smoke_details'] if smoke else cfg['details'];base=cfg['base_ms']
    def ts(ms):return (datetime(1970,1,1,tzinfo=timezone.utc)+timedelta(milliseconds=ms)).isoformat(timespec='milliseconds').replace('+00:00','Z')
    parents=[[uuid7(base+i//10,rng),'parent-'+str(i),ts(base+i//10)] for i in range(np)]
    details=[[uuid7(base+1000+i//10,rng),parents[rng.randrange(np)][0],rng.randrange(1,100001),ts(base+1000+i//10)] for i in range(nd)]
    totals={p[0]:[0,0] for p in parents}
    for d in details:totals[d[1]][0]+=1;totals[d[1]][1]+=d[2]
    files={'schema.txt':SCHEMA,'parents-query.txt':PQUERY,'details-query.txt':DQUERY,'join-query.txt':JOIN,'join-noindex.txt':NOINDEX,'aggregate-query.txt':AGG}
    for label,header,rows in [('parents',['id','label','created_at'],parents),('details',['id','parent_id','cents','created_at'],details)]:
        buf=io.StringIO(newline='');w=csv.writer(buf,lineterminator='\r\n');w.writerow(header);w.writerows(rows);files[label+'.csv']=buf.getvalue()
        files[label+'-expected.txt']=b''.join(wire(x) for x in sorted(rows)).decode()
        statements=[]
        for row in rows:
            values=[str(x) if isinstance(x,int) else "'"+x.replace("'","''")+"'" for x in row]
            statements.append('INSERT INTO uuid_'+('parent' if label=='parents' else 'detail')+' VALUES('+','.join(values)+');\n')
        files[label+'-insert.txt']='BEGIN;\n'+''.join(statements)+'COMMIT;\n'
    cli="SELECT 'T:'||lower(hex(d.id))||char(9)||'T:'||lower(hex(p.id))||char(9)||'I:'||CAST(d.cents AS TEXT)||char(9)||'T:'||lower(hex(d.created_at)) FROM uuid_detail d INDEXED BY detail_parent_time JOIN uuid_parent p ON p.id=d.parent_id ORDER BY d.id;\n"
    files['join-cli-index.txt']='.bail on\n.mode list\n.headers off\n'+cli
    files['join-cli-noindex.txt']=files['join-cli-index.txt'].replace(' INDEXED BY detail_parent_time',' NOT INDEXED')
    files['foreign-keys-readback.txt']='PRAGMA foreign_keys;\n'
    files['negative-orphan.txt']="INSERT INTO uuid_detail VALUES('00000000-0000-7000-8000-000000000001','00000000-0000-7000-8000-000000000002',1,'2025-01-01T00:00:00.000Z');\n"
    files['negative-duplicate.txt']="INSERT INTO uuid_parent VALUES('"+parents[0][0]+"','duplicate','"+parents[0][2]+"');\n"
    files['join-expected.txt']=b''.join(wire(x) for x in sorted(details)).decode()
    files['aggregate-expected.txt']=b''.join(wire([p[0],*totals[p[0]]]) for p in sorted(parents)).decode()
    return files,{'parents':np,'details':nd,'seed':seed,'smoke':smoke,'representation':'UUIDv7 TEXT36','fixed_ms':base}
