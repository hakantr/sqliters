#!/usr/bin/env python3
"""External Python controls; never invokes Rust or performance measurement."""
import copy
import json
from pathlib import Path
import sys
import unittest
sys.dont_write_bytecode=True
from unittest.mock import patch
from support import ROOT
from freeze_inputs import verify
from native import wire

class Controls(unittest.TestCase):
    @classmethod
    def setUpClass(cls):cls.m=verify()
    def test_empty_input_rejected(self):
        m=copy.deepcopy(self.m);m['files']=[]
        with self.assertRaises(ValueError):verify(m)
    def test_duplicate_path_rejected(self):
        m=copy.deepcopy(self.m);m['files'].append(m['files'][0])
        with self.assertRaises(ValueError):verify(m)
    def test_missing_wide_rejected(self):
        m=copy.deepcopy(self.m);del m['datasets']['wide-4096']
        with self.assertRaises(ValueError):verify(m)
    def test_missing_normal_seed_rejected(self):
        m=copy.deepcopy(self.m);del m['datasets']['20261122-full']
        with self.assertRaises(ValueError):verify(m)
    def test_stale_hash_rejected(self):
        m=copy.deepcopy(self.m);m['files'][0]['sha256']='0'*64
        with self.assertRaises(ValueError):verify(m)
    def test_source_drift_rejected(self):
        m=copy.deepcopy(self.m);m['sources']['native.py']='0'*64
        with self.assertRaises(ValueError):verify(m)
    def test_physical_row_contract_rejected(self):
        m=copy.deepcopy(self.m);m['datasets']['wide-2000']['rows']=1
        with self.assertRaises(ValueError):verify(m)
    def test_types_not_sql_equal(self):
        self.assertNotEqual(wire([1]),wire([1.0]));self.assertNotEqual(wire([None]),wire([b'']));self.assertNotEqual(wire(['']),wire([b'']))
    def test_uuid7_rfc9562_a6(self):
        import uuid
        from uuid_family import uuid7
        class Bits:
            def __init__(self):self.v=iter([0xCC3,0x18C4DC0C0C07398F])
            def getrandbits(self,n):return next(self.v)
        value=uuid7(1645557742000,Bits());self.assertEqual(value,'017f22e2-79b0-7cc3-98c4-dc0c0c07398f')
        p=uuid.UUID(value);self.assertEqual(p.version,7);self.assertEqual(p.variant,uuid.RFC_4122);self.assertEqual(p.int>>80,1645557742000)
        if sys.version_info>=(3,14):self.assertEqual(p.time,1645557742000)
    def test_uuid4_wrong_version_rejected(self):
        from live import validate_uuid
        with self.assertRaises(ValueError):validate_uuid('017f22e2-79b0-7cc3-98c4-dc0c0c07398f',4)
    def test_frozen_uuid_files_fk_and_uuid(self):
        import csv,uuid
        for seed in self.m['config']['seeds']:
            d=self.m['datasets'][f'uuid7-{seed}-full'];folder=ROOT/d['folder']
            with (folder/'parents.csv').open(newline='') as f:parents=list(csv.DictReader(f))
            with (folder/'details.csv').open(newline='') as f:details=list(csv.DictReader(f))
            ids={p['id'] for p in parents};self.assertEqual(len(ids),3000);self.assertEqual(len(details),12000)
            self.assertEqual(len({x['id'] for x in parents+details}),15000)
            for row in parents+details:
                p=uuid.UUID(row['id']);self.assertEqual(p.version,7);self.assertEqual(p.variant,uuid.RFC_4122);self.assertEqual(str(p),row['id'])
            self.assertTrue(all(d['parent_id'] in ids for d in details))
    def test_live_requested_payload_is_oracle(self):
        from live import expected_from_request
        with self.assertRaises(ValueError):expected_from_request('parent','db-default-uuid4-utc',['requested'],['id','wrong','ts'])
        with self.assertRaises(ValueError):expected_from_request('detail','db-default-uuid4-utc',['parent',10],['id','parent',11,'ts'])
        with self.assertRaises(ValueError):expected_from_request('detail','client-uuid4-utc',['id','p',10,'ts'],['id','p',11,'ts'])
    def test_full_measurement_matrix_is_222(self):
        from analysis import expected_keys
        from correctness import plan
        self.assertEqual(len(expected_keys(self.m['config'])),222)
        self.assertEqual(len(plan(self.m)),168)
        self.assertEqual(len({x['id'] for x in plan(self.m)}),168)
    def test_duplicate_raw_sample_rejected(self):
        from analysis import summarize
        row={'family':'ingest','case':'cli-import','seed':20261120,'session':0,'repeat':0}
        with self.assertRaises(ValueError):summarize([row,row],self.m['config'],complete=False)
    def test_bulk_average_is_not_row_p95(self):
        from analysis import stats
        self.assertAlmostEqual(stats([10,20,100])['p95'],92.0)
        self.assertEqual(stats([])['n'],0)
    def test_candidate_typed_parse_strict(self):
        from wireio import normalize_row
        self.assertEqual(normalize_row(b'I:1\tRDEC:1.25\tT:00ff\tB:\tN\n',5),b'I:1\tR:3ff4000000000000\tT:00ff\tB:\tN\n')
        for raw in [b'I:9223372036854775808\n',b'RDEC:inf\n',b'T:0\n',b'NULL\n',b'I:1\tN\n']:
            with self.assertRaises(ValueError):normalize_row(raw,1)
    def test_missing_full_C_golden_bundle_rejected(self):
        from compare_cli import baseline_bundle
        with self.assertRaises(ValueError):baseline_bundle(ROOT/'artifacts/nonexistent-baseline-bundle',self.m)
    def test_fake_PASS_is_not_actual_baseline(self):
        from support import ART,out,save
        from audit import audit
        folder=ART/'harness/fake-PASS';out(folder/'placeholder');folder.mkdir(exist_ok=True);save(folder/'status.json',{'status':'PASS_SCOPED'})
        with self.assertRaises((ValueError,KeyError)):audit(folder)
    def test_worker_requires_active_reviewed_ticket(self):
        from support import ART,save
        from gates import worker
        path=ART/'harness/fake-ticket.json';save(path,{'status':'RUNNING','reviewed_commit':'other'})
        with self.assertRaises(ValueError):worker({'mode':'timed','reviewed_commit':'requested','run_state':str(path)})
    def test_live_orphan_cannot_hide_in_empty_fk_diagnostic(self):
        from live import validate_recorded
        from datetime import datetime,timezone
        stamp='2025-01-01T00:00:00.000Z';parent=['00000000-0000-4000-8000-000000000001','p',stamp];child=['00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000003',1,stamp]
        events=[{'table':'parent','mode':'client-uuid4-utc','requested':parent,'returned':parent,'expected_row':parent},{'table':'detail','mode':'client-uuid4-utc','requested':child,'returned':child,'expected_row':child}]
        snapshot={'parents':[parent],'details':[child],'fk':[],'indexed':[[child[0],child[1],1]],'unindexed':[[child[0],child[1],1]]}
        t=datetime(2025,1,1,tzinfo=timezone.utc)
        with self.assertRaises(ValueError):validate_recorded(snapshot,events,t,t,1,1,2)
    def test_port_cwd_relative_output_stays_ignored_and_absolute(self):
        import os
        from support import ART,out
        previous=Path.cwd()
        try:
            os.chdir(ROOT.parent)
            target=out(Path('sqltest/artifacts/external/harness/relative-output/control.txt'))
            self.assertTrue(target.is_absolute());self.assertEqual(target,ART/'harness/relative-output/control.txt');self.assertTrue(target.is_relative_to(ART))
            target.write_text('control only; not benchmark data\n')
        finally:os.chdir(previous)
    def test_uuid_cli_requires_actual_connection_readback(self):
        from wireio import query_payload
        self.assertEqual(query_payload(b'wal\nwal\n1\n0\nSQLTEST_QUERY_BEGIN\nI:1\n'),b'I:1\n')
        with self.assertRaises(ValueError):query_payload(b'wal\n2\n1000\nSQLTEST_QUERY_BEGIN\nI:1\n')
        with self.assertRaises(ValueError):query_payload(b'I:1\n')
    def test_failed_admission_cannot_leave_global_PASS(self):
        from support import ART,out,save
        from benchmark import run
        import shutil
        folder=ART/'harness/admission-failure'
        if folder.exists():shutil.rmtree(out(folder))
        pointer=ROOT/'results/external-summary.json';before=pointer.read_bytes() if pointer.exists() else None
        try:
            save(pointer,{'status':'PASS_SCOPED','old_bundle':'preserve old artifacts'})
            with patch('benchmark.admission',side_effect=ValueError('deliberate invalid admission')):
                with self.assertRaises(ValueError):run('invalid',folder)
            self.assertEqual(json.loads(pointer.read_text())['status'],'FAIL')
            self.assertEqual(json.loads((folder/'status.json').read_text())['status'],'FAIL')
        finally:
            if before is None:pointer.unlink(missing_ok=True)
            else:pointer.write_bytes(before)
    def test_future_missing_backend_explicit(self):
        from external import compatible_family
        self.assertFalse(compatible_family('c-api-prepared-batch','cli-process'))
        self.assertTrue(compatible_family('cli-process','cli-process'))

if __name__=='__main__':
    from support import save,sources,digest
    report=ROOT/'results/external-controls.json'
    save(report,{'status':'RUNNING','performance_status':'NOT_RUN'})
    program=unittest.main(exit=False)
    result=program.result
    save(report,{'status':'PASS' if result.wasSuccessful() else 'FAIL','tests':result.testsRun,'errors':len(result.errors),'failures':len(result.failures),'performance_status':'NOT_RUN','sources':sources(),'input_manifest_sha256':digest(ROOT/'external-input-manifest.json')})
    raise SystemExit(0 if result.wasSuccessful() else 1)
