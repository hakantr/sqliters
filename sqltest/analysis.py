"""Deterministic statistics from raw samples; no trust in PASS/summary labels."""
import math
import statistics


def percentile(xs,p):
    if not xs:return None
    x=sorted(xs);index=(len(x)-1)*p;lo=int(index);hi=min(lo+1,len(x)-1)
    return x[lo]+(x[hi]-x[lo])*(index-lo)


def stats(xs):
    if not xs:return {'n':0,'median':None,'p95':None,'min':None,'max':None}
    if any(not isinstance(x,(int,float)) or not math.isfinite(x) or x<0 for x in xs):raise ValueError('invalid duration')
    return {'n':len(xs),'median':statistics.median(xs),'p95':percentile(xs,.95),'min':min(xs),'max':max(xs)}


def expected_keys(cfg):
    keys={("ingest",c,seed,s,r) for c in cfg['timed_cases'] for seed in cfg['seeds'] for s in range(cfg['sessions']) for r in range(cfg['repeats'])}
    keys|={("uuid-query",c,seed,s,r) for c in cfg['frozen_uuid7']['cli_query_timing']['cases'] for seed in cfg['seeds'] for s in range(cfg['sessions']) for r in range(cfg['repeats'])}
    keys|={("live",mode+'-'+policy,None,s,r) for mode in cfg['live']['cases'] for policy in cfg['live']['commit_modes'] for s in range(cfg['sessions']) for r in range(cfg['repeats'])}
    return keys


def key(row):return (row['family'],row['case'],row.get('seed'),row['session'],row['repeat'])


def summarize(rows,cfg,*,complete=True):
    keys=[key(row) for row in rows]
    if len(keys)!=len(set(keys)):raise ValueError('duplicate sample key')
    if complete and set(keys)!=expected_keys(cfg):raise ValueError('missing/extra sample matrix')
    if not set(keys)<=expected_keys(cfg):raise ValueError('unexpected sample')
    groups={}
    for row in rows:
        if row['status']!='PASS':raise ValueError('failed timing sample')
        name=row['family']+'/'+row['case']+'/'+str(row.get('seed'))
        groups.setdefault(name,[]).append(row)
    result={}
    for name,items in sorted(groups.items()):
        result[name]={'samples':len(items),'total_ns':stats([x['ns'] for x in items]),
          'sample_average_ns_per_record':stats([x['ns']/x['rows'] for x in items]),
          'actual_autocommit_row_latency_ns':stats([n for x in items for n in x.get('row_latency_ns',[])]),
          'batch_latency_ns':stats([n for x in items for n in x.get('batch_latency_ns',[])]),
          'generation_ns':stats([x['generation_ns'] for x in items if x.get('generation_ns') is not None]),
          'input_read_decode_ns':stats([x['input_read_decode_ns'] for x in items if x.get('input_read_decode_ns') is not None]),
          'prepare_ns':stats([x['prepare_ns'] for x in items if x.get('prepare_ns') is not None]),
          'db_write_ns':stats([x['db_write_ns'] for x in items if x.get('db_write_ns') is not None]),
          'commit_latency_ns':stats([n for x in items for n in x.get('commit_latency_ns',[])])}
    return result
