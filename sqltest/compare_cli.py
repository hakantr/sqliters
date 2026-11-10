"""Future candidate comparison through its OWN CLI SELECTs. Never opens a C DB."""
import json
from pathlib import Path
import subprocess
from support import ROOT,ART,out,save,digest,command
from freeze_inputs import DATA,verify
from wireio import normalize_file


def classify(error):
    text=(error.stderr.decode(errors='replace') if isinstance(error,subprocess.CalledProcessError) else str(error)).lower()
    return 'NOT_SUPPORTED' if any(s in text for s in ['unknown command','unknown option','unknown mode','unsupported mode','unsupported command','no such function','no such module','unsupported feature']) else 'DIFF'


def baseline_bundle(path,m):
    from audit import audit
    path=Path(path)
    if not (path/'status.json').exists():raise ValueError('C baseline/golden bundle missing; no regeneration allowed')
    audit(path)
    state=json.loads((path/'status.json').read_text())
    if state['input_manifest_sha256']!=digest(ROOT/'external-input-manifest.json'):raise ValueError('baseline input mismatch')
    return state


def own(binary,db,script,raw,destination,fields):
    result=command([str(binary),str(db)],input=Path(script).read_bytes())
    out(raw).write_bytes(result.stdout);out(raw.with_suffix('.stderr')).write_bytes(result.stderr)
    if result.stderr:raise ValueError('candidate stderr: '+result.stderr.decode(errors='replace'))
    if Path(script).name.startswith('join-cli-'):
        from wireio import query_payload
        typed=Path(str(raw)+'.typed');out(typed).write_bytes(query_payload(result.stdout));return normalize_file(typed,destination,fields)
    return normalize_file(raw,destination,fields)


def exercise(binary,m,output,*,smoke=False,baseline=None):
    from external import cli,persistent_cli
    binary=Path(binary).resolve()
    if not binary.is_file():raise ValueError('candidate binary missing')
    output=out(Path(output));output.mkdir(exist_ok=False)
    save(output/'comparison.json',{'status':'RUNNING','engine':'sqlite-selfcheck' if smoke else 'candidate'})
    results=[];build={'cli':str(binary)}
    try:
        if not smoke and baseline is None:raise ValueError('actual full C baseline bundle required')
        for seed in (m['config']['seeds'][:1] if smoke else m['config']['seeds']):
            for mode in ['import-raw','import-typed','read-auto','read-batch','persistent-auto','persistent-batch']:
                label='preflight' if smoke else 'single' if mode.endswith('auto') else 'full';data=ROOT/m['datasets'][f'{seed}-{label}']['folder'];folder=output/f'{seed}-{mode}';folder.mkdir();db=folder/'candidate.db'
                try:
                    if mode.startswith('persistent'):
                        _,acks=persistent_cli(binary,db,data,mode.split('-')[1],timed=False)
                        if not acks or acks[-1].strip()!=str(m['datasets'][f'{seed}-{label}']['rows']).encode():raise ValueError('candidate ACK count')
                    elif mode=='import-raw':
                        cli(build,db,data/'import.txt',folder/'load.stdout')
                        raw=own(binary,db,DATA/'cli-raw-verify.txt',folder/'raw.stdout',folder/'raw.wire',11)
                        if raw['sha256']!=digest(data/'raw-expected.txt'):raise ValueError('candidate raw CSV roundtrip differs')
                        cli(build,db,DATA/'cli-transform-only.txt',folder/'transform.stdout')
                    else:cli(build,db,data/(mode+'.txt'),folder/'load.stdout')
                    item=own(binary,db,DATA/'cli-verify.txt',folder/'own-select.stdout',folder/'canonical.wire',7)
                    if item['sha256']!=digest(data/'expected.txt') or item['rows']!=m['datasets'][f'{seed}-{label}']['rows']:raise ValueError('candidate typed rows differ')
                    if baseline is not None:
                        matching=[r for r in baseline['samples'] if r['family']=='ingest' and r['seed']==seed and r['case']==('cli-import' if mode.startswith('import') else 'cli-'+mode)]
                        if not matching or any(r['canonical']['sha256']!=item['sha256'] for r in matching):raise ValueError('candidate not equal to actual C canonical baseline')
                    results.append({'case':mode,'seed':seed,'status':'PASS','rows':item['rows'],'canonical_sha256':item['sha256']})
                except Exception as e:results.append({'case':mode,'seed':seed,'status':classify(e),'error':str(e),'stderr':e.stderr.decode(errors='replace') if isinstance(e,subprocess.CalledProcessError) else None})
            data=ROOT/m['datasets'][f'uuid7-{seed}-'+('smoke' if smoke else 'full')]['folder'];folder=output/f'{seed}-uuid';folder.mkdir();db=folder/'candidate.db'
            try:
                cli(build,db,data/'load-cli.txt',folder/'load.stdout')
                for case,leaf in [('join-index','join-cli-index.txt'),('join-noindex','join-cli-noindex.txt')]:
                    item=own(binary,db,data/leaf,folder/(case+'.stdout'),folder/(case+'.wire'),4)
                    if item['sha256']!=digest(data/'join-expected.txt') or item['rows']!=m['config']['frozen_uuid7']['smoke_details' if smoke else 'details']:raise ValueError('candidate UUID own SELECT differs')
                    if baseline is not None:
                        matching=[r for r in baseline['samples'] if r['family']=='uuid-query' and r['seed']==seed and r['case']==case]
                        if not matching or any(r['canonical']['sha256']!=item['sha256'] for r in matching):raise ValueError('candidate UUID differs from actual C baseline')
                    results.append({'case':'uuid-'+case,'seed':seed,'status':'PASS','rows':item['rows'],'canonical_sha256':item['sha256']})
            except Exception as e:results.append({'case':'uuid-queries','seed':seed,'status':classify(e),'error':str(e)})
        result={'status':'PASS_SCOPED' if results and all(r['status']=='PASS' for r in results) else 'DIFF_OR_INCOMPLETE','scope':'small C CLI selfcheck' if smoke else 'candidate OWN CLI results vs actual C frozen baseline','binary_sha256':digest(binary),'input_manifest_sha256':digest(ROOT/'external-input-manifest.json'),'cases':results,'timing':'NOT_RUN','core_API_catalog':'NOT_RUN: matching variable-result/error-stage API contract required','native_API':'NOT_RUN: compatible native backend required','LIVE':'NOT_RUN: same-run API backend required','C_loaded_by_comparator':False}
        save(output/'comparison.json',result)
        return result
    except BaseException as e:save(output/'comparison.json',{'status':'FAIL','error':str(e),'cases':results});raise


def compare(binary,output,baseline):
    m=verify();bundle=baseline_bundle(baseline,m);return exercise(binary,m,output,baseline=bundle)
