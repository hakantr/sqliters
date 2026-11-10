#!/usr/bin/env python3
"""Pinned local C SQLite only. No Rust/Cargo or writes into original sources."""
import json
from pathlib import Path
import shutil
import sys
sys.dont_write_bytecode = True
from support import ROOT, ART, digest, out, save, command, environment

SOURCE = ROOT.parent.parent / 'sqlitesimd/sqlite'
UUID = '555e07ef33f69a0957d24d8e555b702464a131d6f1607f4c3c1e4551e8e6ca76'
FLAGS = ['-O2','-fPIC','-DSQLITE_THREADSAFE=1','-DSQLITE_ENABLE_FTS5',
         '-DSQLITE_ENABLE_RTREE','-DSQLITE_ENABLE_MATH_FUNCTIONS']


def tree(path):
    result={}
    for p in sorted(path.rglob('*')):
        if '.git' in p.relative_to(path).parts:continue
        if p.is_symlink() or not p.resolve().is_relative_to(path.resolve()):raise ValueError('source symlink/outside file')
        if p.is_file():result[str(p.relative_to(path))]=digest(p)
    return result

def tools_identity():
    return {name:{'path':str(Path('/usr/bin/'+name).resolve()),'sha256':digest(Path('/usr/bin/'+name).resolve()),'version':command([name,'--version']).stdout.decode(errors='replace').splitlines()[0] if name=='make' else command([name],input=b'puts [info patchlevel]\n').stdout.decode().strip()} for name in ['make','tclsh']}


def build():
    status = ART/'build.json'
    save(status, {'status':'RUNNING'})
    try:
        if (SOURCE/'VERSION').read_text().strip() != '3.54.1' or (SOURCE/'manifest.uuid').read_text().strip() != UUID:
            raise ValueError('pinned C source identity mismatch')
        before = tree(SOURCE)
        snapshot = ART/'sqlite-src'
        if not snapshot.exists():
            out(snapshot/'placeholder')
            shutil.copytree(SOURCE, snapshot, dirs_exist_ok=True, ignore=shutil.ignore_patterns('.git'))
        if tree(snapshot) != before:
            raise ValueError('C snapshot mismatch; do not reuse unknown generated sources')
        amalg = ART/'amalg'; out(amalg/'placeholder'); amalg.mkdir(exist_ok=True)
        generation = ['make','-f',snapshot/'Makefile.linux-generic',f'TOP={snapshot}','sqlite3.c','sqlite3.h','shell.c']
        log = command(generation, cwd=amalg)
        out(ART/'amalg.log').write_bytes(log.stdout+log.stderr)
        cc = Path('/usr/bin/cc').resolve()
        base = {'source_files':before,'compiler':command([cc,'--version']).stdout.decode().splitlines()[0],
                'compiler_sha256':digest(cc),'environment':environment(), 'flags':FLAGS,
                'generated_sources':{x:digest(amalg/x) for x in ['sqlite3.c','sqlite3.h','shell.c']}, 'generation_command':[str(x) for x in generation], 'generation_tools':tools_identity()}
        variants = {}
        for maximum in [128,2000,4096]:
            folder = ART/f'build/max-{maximum}'; out(folder/'placeholder'); folder.mkdir(exist_ok=True)
            obj, lib, cli = folder/'sqlite3.o',folder/'libsqlite3.so',folder/'sqlite3'
            flags = FLAGS + [f'-DSQLITE_MAX_COLUMN={maximum}']
            commands = [[cc,*flags,'-c',amalg/'sqlite3.c','-o',obj],
                        [cc,'-shared',obj,'-ldl','-lm','-pthread','-o',lib],
                        [cc,*flags,amalg/'shell.c',obj,'-ldl','-lm','-pthread','-o',cli]]
            signature = {**base,'maximum':maximum,'commands':[[str(x) for x in c] for c in commands]}
            cache = folder/'identity.json'
            previous = json.loads(cache.read_text()) if cache.exists() else {}
            reusable = previous.get('signature') == signature and all(p.exists() and previous.get('binaries',{}).get(p.name)==digest(p) for p in [obj,lib,cli])
            if not reusable:
                for i,c in enumerate(commands):
                    print(f'C build MAX_COLUMN={maximum} phase={i+1}/3',flush=True)
                    result=command(c)
                    out(folder/f'phase-{i}.log').write_bytes(result.stdout+result.stderr)
                save(cache, {'signature':signature,'binaries':{p.name:digest(p) for p in [obj,lib,cli]}})
            source_id=command([cli,':memory:','SELECT sqlite_source_id();']).stdout.decode().strip()
            if UUID not in source_id:
                raise ValueError('built C source id mismatch')
            variants[str(maximum)]={'maximum':maximum,'library':str(lib),'cli':str(cli),
                'library_sha256':digest(lib),'cli_sha256':digest(cli),'identity_sha256':digest(cache),
                'source_id':source_id,'compile_options':command([cli,':memory:','PRAGMA compile_options;']).stdout.decode().splitlines()}
        if tree(SOURCE)!=before:
            raise ValueError('original C source drift')
        report={'status':'PASS','scope':'C build identity only; no benchmark','uuid':UUID,
                'variants':variants,'source_tree_sha256':__import__('hashlib').sha256(json.dumps(before,sort_keys=True).encode()).hexdigest()}
        save(status,report); return report
    except BaseException as e:
        save(status,{'status':'FAIL','error':str(e)});raise


def verified(maximum=2000):
    report=json.loads((ART/'build.json').read_text())
    if report.get('status')!='PASS' or report.get('uuid')!=UUID:raise ValueError('missing verified pinned build')
    row=report['variants'][str(maximum)]
    for name in ['library','cli']:
        if digest(row[name])!=row[name+'_sha256']:raise ValueError('C binary drift')
    identity=json.loads((Path(row['cli']).parent/'identity.json').read_text())
    if digest(Path(row['cli']).parent/'identity.json')!=row['identity_sha256'] or tree(SOURCE)!=identity['signature']['source_files']:
        raise ValueError('C source/build identity drift')
    sig=identity['signature'];folder=Path(row['cli']).parent;amalg=ART/'amalg';cc=Path('/usr/bin/cc').resolve();obj=folder/'sqlite3.o'
    expected_commands=[[cc,*FLAGS,f'-DSQLITE_MAX_COLUMN={maximum}','-c',amalg/'sqlite3.c','-o',obj],[cc,'-shared',obj,'-ldl','-lm','-pthread','-o',folder/'libsqlite3.so'],[cc,*FLAGS,f'-DSQLITE_MAX_COLUMN={maximum}',amalg/'shell.c',obj,'-ldl','-lm','-pthread','-o',folder/'sqlite3']]
    if sig['maximum']!=maximum or sig['flags']!=FLAGS or sig['commands']!=[[str(x) for x in c] for c in expected_commands] or sig['generation_tools']!=tools_identity() or sig['generated_sources']!={x:digest(amalg/x) for x in ['sqlite3.c','sqlite3.h','shell.c']}:raise ValueError('build flags/tools/generated-source drift')
    if identity['signature']['environment']!=environment() or identity['signature']['compiler_sha256']!=digest(Path('/usr/bin/cc').resolve()):
        raise ValueError('compiler/build environment drift')
    return row


if __name__=='__main__':build()
