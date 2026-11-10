#!/usr/bin/env python3
"""External Python controls; never invokes Rust or performance measurement."""
import copy
import json
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
    def test_future_missing_backend_explicit(self):
        from external import compatible_family
        self.assertFalse(compatible_family('c-api-prepared-batch','cli-process'))
        self.assertTrue(compatible_family('cli-process','cli-process'))

if __name__=='__main__':unittest.main()
