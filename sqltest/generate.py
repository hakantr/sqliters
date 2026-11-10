#!/usr/bin/env python3
"""Produce static SQL/CSV/JSONL only. No engine, subprocess, test, or build."""
import csv
import hashlib
import io
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
MASK = (1 << 64) - 1


def sha(data):
    return hashlib.sha256(data).hexdigest()


def sql_text(value):
    if value is None:
        return 'NULL'
    if '\x00' in value:
        return "CAST(X'" + value.encode('utf-8').hex() + "' AS TEXT)"
    return "'" + value.replace("'", "''") + "'"


def integer(value):
    return 'NULL' if value is None else str(value)


def guarded_write(path, data):
    if not path.resolve().is_relative_to(ROOT) or path.is_symlink() or any(parent.is_symlink() for parent in path.parents):
        raise ValueError(f'outside/symlink output forbidden: {path}')
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)


def csv_value(value):
    if value is None:
        return '\\N'
    if isinstance(value, str):
        return value.replace('\\', '\\\\').replace('\x00', '\\0').replace('\n', '\\n').replace('\r', '\\r')
    return value


class SplitMix64:
    def __init__(self, seed):
        self.state = seed & MASK

    def next(self):
        self.state = (self.state + 0x9e3779b97f4a7c15) & MASK
        z = self.state
        z = ((z ^ (z >> 30)) * 0xbf58476d1ce4e5b9) & MASK
        z = ((z ^ (z >> 27)) * 0x94d049bb133111eb) & MASK
        return (z ^ (z >> 31)) & MASK


def main():
    profiles = json.loads((ROOT / 'profiles.json').read_text())
    entries = []
    scenarios = []

    def put(relative, text, **details):
        path = ROOT / relative
        data = text.encode('utf-8')
        guarded_write(path, data)
        entry = {'path': relative, 'bytes': len(data), 'sha256': sha(data),
                 'physical_lines': len(data.splitlines()), 'execution_status': 'NOT_RUN', **details}
        entries.append(entry)
        return entry

    def scenario(name, family, sql, expectation='success expected; unverified', **details):
        path = 'examples/scenarios/' + name + '.sql'
        put(path, '-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.\n' + sql.strip() + '\n',
            kind='readable_scenario', family=family)
        scenarios.append({'id': name, 'family': family, 'sql_path': path,
                          'prerequisites': ['examples/schema.sql', 'examples/seed.sql'],
                          'expectation': expectation, 'execution_status': 'NOT_RUN', **details})

    schema = '''PRAGMA foreign_keys=ON;
CREATE TABLE customers(id INTEGER PRIMARY KEY,name TEXT NOT NULL,city TEXT);
CREATE TABLE orders(id INTEGER PRIMARY KEY,customer_id INTEGER,total REAL,note TEXT);
CREATE TABLE items(id INTEGER PRIMARY KEY,order_id INTEGER,sku TEXT,qty INTEGER,price REAL);
CREATE TABLE employees(id INTEGER PRIMARY KEY,manager_id INTEGER,name TEXT,dept TEXT);
CREATE TABLE join_keys(k INTEGER,label TEXT);
CREATE TABLE inventory(sku TEXT PRIMARY KEY,qty INTEGER CHECK(qty>=0),price REAL);
CREATE TABLE fk_parent(id INTEGER PRIMARY KEY);
CREATE TABLE fk_child(id INTEGER PRIMARY KEY,pid INTEGER REFERENCES fk_parent(id) ON DELETE CASCADE DEFERRABLE INITIALLY DEFERRED);
CREATE TABLE audit(event TEXT,entity_id INTEGER);
'''
    seed = '''BEGIN;
INSERT INTO customers VALUES(1,'Ada','İstanbul'),(2,'O''Brien','東京'),(3,'Zoë',NULL),(4,'Unmatched','');
INSERT INTO orders VALUES(10,1,12.5,'paid'),(11,1,0.0,NULL),(12,2,19.75,'pending'),(13,NULL,7.0,'orphan'),(14,99,4.0,'unknown');
INSERT INTO items VALUES(101,10,'A',2,3.0),(102,10,'B',1,6.5),(103,12,'A',3,6.5),(104,NULL,'Z',1,1.0);
INSERT INTO employees VALUES(1,NULL,'Lead','R&D'),(2,1,'Dev A','R&D'),(3,1,'Dev B','R&D'),(4,2,'Intern',NULL);
INSERT INTO join_keys VALUES(1,'dup-a'),(1,'dup-b'),(NULL,'null-key'),(77,'unmatched-key');
INSERT INTO inventory VALUES('A',10,3.0),('B',5,6.5);
INSERT INTO fk_parent VALUES(1),(2);
INSERT INTO fk_child VALUES(1,1),(2,1),(3,2);
COMMIT;
'''
    put('examples/schema.sql', schema, kind='fixture_schema')
    put('examples/seed.sql', seed, kind='fixture_data', literal_insert_rows=28)

    # Every scenario is independent: mutation and intentional error cases never
    # rely on the preceding scenario having executed or rolled back.
    definitions = [
        ('join-inner','join','SELECT c.id,c.name,o.id,o.total FROM customers c INNER JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;'),
        ('join-left','join','SELECT c.id,o.id FROM customers c LEFT JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;'),
        ('join-right','join','SELECT c.id,o.id FROM customers c RIGHT JOIN orders o ON o.customer_id=c.id ORDER BY o.id,c.id;'),
        ('join-full','join','SELECT c.id,o.id FROM customers c FULL JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;'),
        ('join-cross','join','SELECT c.id,j.k,j.label FROM customers c CROSS JOIN join_keys j ORDER BY c.id,j.label;'),
        ('join-natural','join',"CREATE TABLE nick(id INTEGER,nick TEXT); INSERT INTO nick VALUES(1,'A'),(2,'B'),(8,'Z'); SELECT id,name,nick FROM customers NATURAL LEFT JOIN nick ORDER BY id;"),
        ('join-self','join','SELECT e.name,m.name AS manager FROM employees e LEFT JOIN employees m ON e.manager_id=m.id ORDER BY e.id;'),
        ('join-using','join','CREATE TABLE keys2(k INTEGER,v TEXT); INSERT INTO keys2 VALUES(1,\'hit\'),(NULL,\'null\'),(8,\'miss\'); SELECT k,label,v FROM join_keys LEFT JOIN keys2 USING(k) ORDER BY label,v;'),
        ('join-semi-exists','join','SELECT c.id FROM customers c WHERE EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;'),
        ('join-anti-not-exists','join','SELECT c.id FROM customers c WHERE NOT EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;'),
        ('join-not-in-null','join','SELECT c.id FROM customers c WHERE c.id NOT IN(SELECT customer_id FROM orders) ORDER BY c.id;'),
        ('join-duplicate-null','join',"SELECT a.label,b.label FROM join_keys a FULL JOIN join_keys b ON a.k=b.k ORDER BY a.label,b.label;"),
        ('join-three-way','join','SELECT c.id,o.id,i.sku FROM customers c JOIN orders o ON c.id=o.customer_id LEFT JOIN items i ON i.order_id=o.id ORDER BY c.id,o.id,i.id;'),
        ('join-natural-after-alter','join',"CREATE TABLE natural_copy(id INTEGER,nick TEXT); INSERT INTO natural_copy VALUES(1,'x'); ALTER TABLE natural_copy ADD COLUMN city TEXT; SELECT id,name,nick FROM customers NATURAL LEFT JOIN natural_copy ORDER BY id;"),
        ('union','set','SELECT customer_id AS k FROM orders UNION SELECT k FROM join_keys ORDER BY k;'),
        ('union-all','set','SELECT customer_id AS k FROM orders UNION ALL SELECT k FROM join_keys ORDER BY k;'),
        ('intersect','set','SELECT customer_id AS k FROM orders INTERSECT SELECT k FROM join_keys ORDER BY k;'),
        ('except','set','SELECT id FROM customers EXCEPT SELECT customer_id FROM orders ORDER BY id;'),
        ('distinct','select','SELECT DISTINCT city FROM customers ORDER BY city;'),
        ('subquery-correlated','select','SELECT c.id,(SELECT count(*) FROM orders o WHERE o.customer_id=c.id) AS n FROM customers c ORDER BY c.id;'),
        ('cte','cte','WITH totals AS(SELECT customer_id,sum(total) AS total FROM orders GROUP BY customer_id) SELECT c.id,t.total FROM customers c LEFT JOIN totals t ON t.customer_id=c.id ORDER BY c.id;'),
        ('cte-recursive','cte','WITH RECURSIVE x(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<25) SELECT n,n*n FROM x ORDER BY n;'),
        ('aggregate','aggregate','SELECT customer_id,count(*),count(note),sum(total),avg(total),min(total),max(total) FROM orders GROUP BY customer_id HAVING count(*)>0 ORDER BY customer_id;'),
        ('group-concat','aggregate',"SELECT group_concat(sku,'|') FROM(SELECT sku FROM items ORDER BY id);"),
        ('window-rank','window','SELECT id,customer_id,row_number() OVER(PARTITION BY customer_id ORDER BY total,id),dense_rank() OVER(ORDER BY total) FROM orders ORDER BY id;'),
        ('window-frame','window','SELECT id,sum(total) OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW),lag(total) OVER(ORDER BY id),lead(total) OVER(ORDER BY id) FROM orders ORDER BY id;'),
        ('window-ties','window','SELECT k,label,rank() OVER(ORDER BY k),sum(k) OVER(ORDER BY k RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM join_keys ORDER BY label;'),
        ('ddl-table','ddl','CREATE TABLE composite(a INTEGER,b TEXT,PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO composite VALUES(1,\'x\'); SELECT * FROM composite; DROP TABLE composite;'),
        ('ddl-strict','ddl',"CREATE TABLE typed(a INTEGER,b TEXT) STRICT; INSERT INTO typed VALUES(1,'x'); SELECT typeof(a),typeof(b) FROM typed;"),
        ('ddl-temp','ddl','CREATE TEMP TABLE scratch(k INTEGER); INSERT INTO scratch VALUES(7); SELECT k FROM temp.scratch; DROP TABLE scratch;'),
        ('ddl-view','ddl','CREATE VIEW customer_totals AS SELECT customer_id,sum(total) AS total FROM orders GROUP BY customer_id; SELECT * FROM customer_totals ORDER BY customer_id; DROP VIEW customer_totals;'),
        ('alter-add-rename-drop','alter','CREATE TABLE mutable(a INTEGER,b TEXT); ALTER TABLE mutable ADD COLUMN c BLOB; ALTER TABLE mutable RENAME COLUMN b TO label; ALTER TABLE mutable DROP COLUMN c; ALTER TABLE mutable RENAME TO renamed; PRAGMA table_info(renamed);'),
        ('insert-default','dml','CREATE TABLE defaults(id INTEGER PRIMARY KEY,v TEXT DEFAULT \'d\'); INSERT INTO defaults DEFAULT VALUES; SELECT * FROM defaults;'),
        ('insert-select','dml','CREATE TABLE selected AS SELECT id,name FROM customers WHERE id<=2; INSERT INTO selected SELECT id,name FROM customers WHERE id=3; SELECT * FROM selected ORDER BY id;'),
        ('update-delete','dml',"UPDATE inventory SET qty=qty+2,price=price*1.125 WHERE sku='A'; DELETE FROM inventory WHERE sku='B'; SELECT * FROM inventory ORDER BY sku;"),
        ('replace','conflict',"REPLACE INTO inventory VALUES('A',99,5.0); SELECT * FROM inventory ORDER BY sku;"),
        ('upsert','conflict',"INSERT INTO inventory VALUES('A',2,3.5) ON CONFLICT(sku) DO UPDATE SET qty=inventory.qty+excluded.qty,price=excluded.price RETURNING sku,qty,price;"),
        ('returning','dml',"UPDATE inventory SET qty=qty+1 RETURNING sku,qty; DELETE FROM inventory WHERE sku='B' RETURNING sku;"),
        ('conflict-ignore','conflict',"INSERT OR IGNORE INTO inventory VALUES('A',1,0.0); SELECT * FROM inventory ORDER BY sku;"),
        ('transaction-rollback','transaction',"BEGIN IMMEDIATE; UPDATE inventory SET qty=qty+100; ROLLBACK; SELECT * FROM inventory ORDER BY sku;"),
        ('transaction-commit','transaction',"BEGIN DEFERRED; UPDATE inventory SET qty=qty+1; END TRANSACTION; SELECT * FROM inventory ORDER BY sku;"),
        ('transaction-savepoint','transaction',"SAVEPOINT outer_sp; UPDATE inventory SET qty=qty+1; SAVEPOINT inner_sp; DELETE FROM inventory; ROLLBACK TO inner_sp; RELEASE inner_sp; RELEASE outer_sp; SELECT * FROM inventory ORDER BY sku;"),
        ('transaction-exclusive','transaction','BEGIN EXCLUSIVE; SELECT count(*) FROM customers; COMMIT;'),
        ('trigger-after','trigger',"CREATE TRIGGER inv_after AFTER UPDATE ON inventory BEGIN INSERT INTO audit VALUES('update',new.qty); END; UPDATE inventory SET qty=qty+1 WHERE sku='A'; SELECT * FROM audit; DROP TRIGGER inv_after;"),
        ('trigger-before','trigger',"CREATE TRIGGER inv_before BEFORE INSERT ON inventory WHEN new.qty<0 BEGIN SELECT RAISE(IGNORE); END; INSERT INTO inventory VALUES('N',-1,0); SELECT * FROM inventory ORDER BY sku;"),
        ('trigger-instead-of','trigger',"CREATE VIEW inv_view AS SELECT sku,qty,price FROM inventory; CREATE TRIGGER view_ins INSTEAD OF INSERT ON inv_view BEGIN INSERT INTO inventory VALUES(new.sku,new.qty,new.price); END; INSERT INTO inv_view VALUES('C',4,2.0); SELECT * FROM inventory ORDER BY sku;"),
        ('fk-cascade','foreign_key','DELETE FROM fk_parent WHERE id=1; SELECT * FROM fk_child ORDER BY id;'),
        ('fk-deferred-savepoint','foreign_key','BEGIN; SAVEPOINT pending; INSERT INTO fk_child VALUES(20,99); ROLLBACK TO pending; RELEASE pending; COMMIT; SELECT * FROM fk_child ORDER BY id;'),
        ('index-composite','index','CREATE INDEX customer_city_name ON customers(city,name); SELECT id FROM customers INDEXED BY customer_city_name WHERE city IS NULL ORDER BY id; DROP INDEX customer_city_name;'),
        ('index-partial-expression','index','CREATE INDEX names_nonnull ON customers(lower(name)) WHERE city IS NOT NULL; SELECT name FROM customers WHERE city IS NOT NULL AND lower(name)=\'ada\';'),
        ('explain','index','EXPLAIN SELECT id FROM orders WHERE customer_id=1; EXPLAIN QUERY PLAN SELECT c.id,o.id FROM customers c JOIN orders o ON c.id=o.customer_id;'),
        ('null-three-valued','value','SELECT NULL=NULL,NULL IS NULL,1 IN(2,NULL),1 NOT IN(2,NULL),coalesce(NULL,7),CASE WHEN NULL THEN 1 ELSE 2 END;'),
        ('types-int64','value',"SELECT -9223372036854775808,9223372036854775807,typeof(9223372036854775807+1),CAST('9223372036854775808' AS INTEGER),typeof(CAST('1.0' AS NUMERIC));"),
        ('types-real-special','value','SELECT -0.0,typeof(-0.0),1e999,-1e999,0.0/0.0,CAST(0.125 AS TEXT);'),
        ('unicode-nul-blob','value',"SELECT 'İstanbul','東京','Zoë','O''Brien',hex(CAST(X'610062' AS TEXT)),length(CAST(X'610062' AS TEXT)),X'',X'00ff80';"),
        ('like-glob-collation','value',"SELECT name FROM customers WHERE name LIKE '%a%' OR name GLOB '*[A-Z]*' ORDER BY name COLLATE NOCASE,name COLLATE BINARY;"),
        ('core-functions','function',"SELECT abs(-7),round(1.125,2),printf('%08d',17),substr('東京-x',2,2),instr('abcabc','bc'),hex(zeroblob(4)),quote(NULL);"),
        ('date-functions','function',"SELECT date('2000-02-28','+1 day'),strftime('%Y-%m-%d','2000-01-01','+1 month'),unixepoch('1970-01-01 00:00:00');"),
        ('math-function-probe','function','SELECT sqrt(16),pow(2,8),log2(8);'),
        ('order-limit','select','SELECT id,total FROM orders ORDER BY total DESC,id LIMIT 3 OFFSET 1;'),
        ('analyze-reindex','management','CREATE INDEX order_customer ON orders(customer_id); ANALYZE; REINDEX order_customer; SELECT count(*) FROM sqlite_stat1;'),
        ('vacuum','management','VACUUM; PRAGMA integrity_check;'),
        ('vacuum-into','management',"-- Future execution only from sqltest cwd; destination must not exist.\nVACUUM INTO 'artifacts/future-vacuum-output.sqlite';"),
        ('attach-detach','management',"ATTACH DATABASE ':memory:' AS aux; CREATE TABLE aux.t(v INTEGER); INSERT INTO aux.t VALUES(9); SELECT * FROM aux.t; DETACH DATABASE aux;"),
        ('pragma-inspection','management','PRAGMA foreign_keys; PRAGMA page_size; PRAGMA encoding; PRAGMA table_info(customers); PRAGMA index_list(inventory); PRAGMA integrity_check;'),
        ('quoted-identifiers','syntax',"CREATE TABLE \"odd table\"(\"select\" INTEGER,\"a\"\"b\" TEXT); INSERT INTO \"odd table\" VALUES(1,'semi;colon -- literal'); SELECT \"select\",\"a\"\"b\" FROM \"odd table\";"),
        ('comments-tail','syntax',"-- Semicolons inside literal/comment must not split SQL.\nSELECT ';' AS punctuation; /* comment ; */ SELECT 'it''s valid text' AS escaped;"),
        ('overflow-values','storage',"CREATE TABLE overflow_data(id INTEGER PRIMARY KEY,t TEXT,b BLOB); INSERT INTO overflow_data VALUES(1,replace(hex(zeroblob(9000)),'00','x'),zeroblob(20000)); SELECT id,length(t),length(b),hex(substr(b,19998,3)) FROM overflow_data; SELECT t,b FROM overflow_data ORDER BY id; PRAGMA integrity_check;"),
    ]
    for name, family, sql in definitions:
        if name == 'math-function-probe':
            scenario(name, family, sql, 'success only if math functions enabled; unverified',
                     required_capabilities=['math_functions'], future_c_compile_flags=['-DSQLITE_ENABLE_MATH_FUNCTIONS'])
        else:
            scenario(name, family, sql)
    negatives = [
        ('negative-syntax','syntax','SELECT FROM customers;', 'syntax error expected'),
        ('negative-unique','conflict',"INSERT INTO inventory VALUES('A',1,1.0);", 'UNIQUE/PRIMARY KEY constraint expected'),
        ('negative-check','conflict',"INSERT INTO inventory VALUES('C',-1,1.0);", 'CHECK constraint expected'),
        ('negative-not-null','conflict','INSERT INTO customers VALUES(99,NULL,NULL);', 'NOT NULL constraint expected'),
        ('negative-fk','foreign_key','INSERT INTO fk_child VALUES(99,999);', 'FOREIGN KEY constraint expected at implicit transaction completion'),
        ('negative-unknown-column','syntax','SELECT missing_column FROM customers;', 'no such column expected'),
        ('negative-strict-type','ddl',"CREATE TABLE strict_negative(v INTEGER) STRICT; INSERT INTO strict_negative VALUES('not-an-integer');", 'datatype constraint expected'),
    ]
    for name, family, sql, expectation in negatives:
        scenario(name, family, sql, expectation, intentionally_negative=True)

    # Static normal data: CSV and JSONL and INSERT SQL contain the same 10k rows.
    rng = SplitMix64(profiles['seed'])
    normal_sql = ['-- Static generated input; no SQL was executed.\nCREATE TABLE sample(id INTEGER PRIMARY KEY,group_id INTEGER,n INTEGER,r REAL,name TEXT,note TEXT,payload BLOB);\nBEGIN;\n']
    csv_buffer = io.StringIO(newline='')
    writer = csv.writer(csv_buffer, lineterminator='\n')
    header = ['id', 'group_id', 'n', 'r', 'name', 'note', 'payload_hex']
    writer.writerow(header)
    json_lines = []
    small_csv = io.StringIO(newline='')
    small_writer = csv.writer(small_csv, lineterminator='\n')
    small_writer.writerow(header)
    edges = [-(1 << 63), (1 << 63) - 1, -1, 0, 1, 127, 128, 32767, -32768, 2147483647, -2147483648]
    names = ["Ada", "O'Brien", 'İstanbul', '東京', 'Zoë', '', 'line\nbreak', 'a\x00b', 'Σσς', 'é', 'e\u0301']
    for row_id in range(1, profiles['normal_records'] + 1):
        x = rng.next()
        n = None if row_id % 13 == 0 else edges[row_id % len(edges)] if row_id % 7 == 0 else (rng.next() & ((1 << 63) - 1)) * (-1 if x & 1 else 1)
        group = None if row_id % 17 == 0 else x % 97
        real = ((rng.next() % 2_000_001) - 1_000_000) / 8.0
        name = names[x % len(names)]
        note = None if row_id % 5 == 0 else 'seed-' + format(x, '016x')
        blob = b'' if row_id % 11 == 0 else rng.next().to_bytes(8, 'big')[:row_id % 9]
        record = {'id': row_id, 'group_id': group, 'n': n, 'r': real, 'name': name, 'note': note,
                  'payload': {'type': 'blob', 'hex': blob.hex()}}
        values = [row_id, group, n, real, name, note, blob.hex()]
        csv_values = [csv_value(value) for value in values]
        writer.writerow(csv_values)
        if row_id <= 12:
            small_writer.writerow(csv_values)
        json_lines.append(json.dumps(record, ensure_ascii=False, separators=(',', ':'), allow_nan=False))
        normal_sql.append('INSERT INTO sample VALUES(' + ','.join([str(row_id), integer(group), integer(n), repr(real), sql_text(name), sql_text(note), "X'" + blob.hex() + "'"]) + ');\n')
    normal_sql.append('COMMIT;\nSELECT id,group_id,n,r,name,note,payload FROM sample ORDER BY id;\n')
    put('artifacts/generated/full/normal.csv', csv_buffer.getvalue(), kind='physical_csv', generated_records=profiles['normal_records'], columns=7)
    put('artifacts/generated/full/normal.jsonl', '\n'.join(json_lines) + '\n', kind='physical_jsonl', generated_records=profiles['normal_records'])
    put('artifacts/generated/full/normal.sql', ''.join(normal_sql), kind='literal_insert_sql', planned_insert_rows=profiles['normal_records'], planned_result_columns=7)
    put('artifacts/generated/full/normal-sample.csv', small_csv.getvalue(), kind='physical_csv', generated_records=12, columns=7)
    put('artifacts/generated/full/normal-sample.jsonl', '\n'.join(json_lines[:12]) + '\n', kind='physical_jsonl', generated_records=12)
    put('artifacts/generated/full/normal-sample.sql', normal_sql[0] + ''.join(normal_sql[1:13]) + normal_sql[-1], kind='literal_insert_sql', planned_insert_rows=12, planned_result_columns=7)

    # A separate large, related fixture. Names match the small readable fixture
    # but this SQL must be prepared in its own future database.
    related = SplitMix64(profiles['seed'] ^ 0x52454c4154494f4e)
    relations = profiles['relational_records']
    relational_sql = [schema.split('CREATE TABLE join_keys', 1)[0],
                      'CREATE TABLE measurements(id INTEGER PRIMARY KEY,device_id INTEGER,at_sec INTEGER,value REAL,note TEXT);\nBEGIN;\n']
    relational_stats = {}
    for table, columns in [('customers',['id','name','city']), ('orders',['id','customer_id','total','note']),
                           ('items',['id','order_id','sku','qty','price']), ('employees',['id','manager_id','name','dept']),
                           ('measurements',['id','device_id','at_sec','value','note'])]:
        buf = io.StringIO(newline='')
        csv_out = csv.writer(buf, lineterminator='\n')
        csv_out.writerow(columns)
        json_out = []
        null_keys = unmatched_keys = 0
        cities = ['İstanbul','東京','Zürich',None,'']
        for row_id in range(1, relations[table]+1):
            x = related.next()
            if table == 'customers':
                values = [row_id, names[x % len(names)] + '-' + str(row_id), cities[(x >> 8) % len(cities)]]
            elif table == 'orders':
                key = None if row_id % 17 == 0 else relations['customers'] + 1 + x % 97 if row_id % 29 == 0 else 1 + x % (relations['customers']-150)
                null_keys += key is None
                unmatched_keys += key is not None and key > relations['customers']
                values = [row_id, key, (x % 200000-10000)/8.0, None if row_id % 7 == 0 else "order-'"+format(x,'016x')]
            elif table == 'items':
                key = None if row_id % 31 == 0 else relations['orders'] + 1 + x % 43 if row_id % 37 == 0 else 1 + x % (relations['orders']-200)
                null_keys += key is None
                unmatched_keys += key is not None and key > relations['orders']
                values = [row_id,key,'sku-'+str(x%211),x%9,(x%100000)/8.0]
            elif table == 'employees':
                key = None if row_id <= 8 else 1+x%(row_id-1)
                null_keys += key is None
                values = [row_id,key,'employee-'+str(row_id),None if row_id%11==0 else ['R&D','销售','ops'][x%3]]
            else:
                key = None if row_id%41==0 else 1+x%73
                null_keys += key is None
                values = [row_id,key,1700000000+row_id//11,None if row_id%19==0 else (x%200001-100000)/8.0,'series-'+str(x%13)]
            csv_out.writerow([csv_value(value) for value in values])
            json_out.append(json.dumps(dict(zip(columns, values)), ensure_ascii=False, separators=(',', ':'), allow_nan=False))
            literals = [sql_text(v) if isinstance(v,str) else 'NULL' if v is None else repr(v) for v in values]
            relational_sql.append(f'INSERT INTO {table} VALUES(' + ','.join(literals) + ');\n')
        relative = 'artifacts/generated/full/relational/' + table
        put(relative+'.csv',buf.getvalue(),kind='physical_csv',dataset='relational',table=table,generated_records=relations[table],columns=len(columns))
        put(relative+'.jsonl','\n'.join(json_out)+'\n',kind='physical_jsonl',dataset='relational',table=table,generated_records=relations[table])
        relational_stats[table] = {'physical_records':relations[table], 'null_relation_keys':null_keys, 'unmatched_relation_keys':unmatched_keys}
    relational_sql.append('COMMIT;\n')
    put('artifacts/generated/full/relational.sql',''.join(relational_sql),kind='literal_insert_sql',dataset='relational',
        planned_insert_rows=sum(relations.values()),tables=relational_stats)
    queries = '''-- Static queries for artifacts/generated/full/relational.sql in its own future DB. NOT_RUN.
-- 01 Full materialization (all fields); no rowcount-only substitute.
SELECT id,name,city FROM customers ORDER BY id;
SELECT id,customer_id,total,note FROM orders ORDER BY id;
SELECT id,order_id,sku,qty,price FROM items ORDER BY id;
SELECT id,manager_id,name,dept FROM employees ORDER BY id;
SELECT id,device_id,at_sec,value,note FROM measurements ORDER BY id;
-- 02 Duplicate/NULL/unmatched relationship keys.
SELECT c.id,o.id,o.total FROM customers c FULL JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;
SELECT o.id,i.id,i.sku FROM orders o LEFT JOIN items i ON i.order_id=o.id ORDER BY o.id,i.id;
SELECT c.id,count(DISTINCT o.id),count(i.id),sum(i.qty*i.price) FROM customers c LEFT JOIN orders o ON o.customer_id=c.id LEFT JOIN items i ON i.order_id=o.id GROUP BY c.id ORDER BY c.id;
SELECT c.id FROM customers c WHERE EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;
SELECT c.id FROM customers c WHERE NOT EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;
-- 03 Acyclic manager graph: parent id is strictly lower than child id.
WITH RECURSIVE tree(id,manager_id,name,depth) AS(SELECT id,manager_id,name,0 FROM employees WHERE manager_id IS NULL UNION ALL SELECT e.id,e.manager_id,e.name,t.depth+1 FROM employees e JOIN tree t ON e.manager_id=t.id) SELECT * FROM tree ORDER BY id;
-- 04 Tied timestamps and deterministic time-series frame.
SELECT id,device_id,at_sec,value,row_number() OVER(PARTITION BY device_id ORDER BY at_sec,id),avg(value) OVER(PARTITION BY device_id ORDER BY at_sec,id ROWS BETWEEN 5 PRECEDING AND CURRENT ROW),lag(value) OVER(PARTITION BY device_id ORDER BY at_sec,id) FROM measurements ORDER BY id;
SELECT device_id,(at_sec/60)*60 AS minute,count(*),min(value),max(value),avg(value) FROM measurements GROUP BY device_id,minute ORDER BY device_id,minute;
'''
    put('examples/large-join-queries.sql',queries,kind='relational_query_set',prerequisite='artifacts/generated/full/relational.sql',planned_select_statements=13)

    for wide in profiles['wide_tables']:
        width, rows = wide['columns'], wide['planned_rows']
        columns = [f'c{i:04}' for i in range(width)]
        expressions = []
        for i in range(width):
            # Small integer arithmetic remains below int64; no SQL random().
            salt = (profiles['seed'] + i * 104729) % 2147483647
            v = f'((i*1103515245+{salt})%2147483647)'
            expressions.append(f"CASE ({v}%11) WHEN 0 THEN NULL WHEN 1 THEN -9223372036854775808 WHEN 2 THEN 9223372036854775807 WHEN 3 THEN ({v}/8.0) WHEN 4 THEN printf('c{i:04}-%08x-東京', {v}) WHEN 5 THEN CAST(printf('%08x',{v}) AS BLOB) WHEN 6 THEN X'' ELSE ({v}-1073741824) END")
        sql = f'-- Static wide input: {width} columns, {rows} planned inserted rows; NOT_RUN.\n'
        sql += f'CREATE TABLE wide_{width}(\n' + ',\n'.join(columns) + '\n);\nBEGIN;\n'
        sql += f'WITH RECURSIVE seq(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM seq WHERE i<{rows})\n'
        sql += f'INSERT INTO wide_{width} SELECT\n' + ',\n'.join(expressions) + '\nFROM seq;\nCOMMIT;\n'
        sql += f'SELECT {",".join(columns)} FROM wide_{width} ORDER BY rowid;\n'
        sql += f'SELECT count(*) FROM wide_{width};\nPRAGMA integrity_check;\n'
        put(f'artifacts/generated/full/wide-{width}.sql', sql, kind='wide_sql', planned_columns=width,
            explicit_ddl_columns=width, explicit_insert_expressions=width, planned_insert_rows=rows,
            planned_materialized_fields=width * rows, future_max_column=wide['future_max_column'])
        first, middle, last = columns[0], columns[width//2], columns[-1]
        workload = f'''-- Future own DB loaded with wide-{width}.sql; NOT_RUN.
BEGIN IMMEDIATE;
UPDATE wide_{width} SET {first}=rowid,{middle}=printf('updated-%08d',rowid),{last}=CAST(printf('%08x',rowid) AS BLOB) WHERE rowid%3=0;
SAVEPOINT discard_delete;
DELETE FROM wide_{width} WHERE rowid%13=0;
ROLLBACK TO discard_delete;
RELEASE discard_delete;
COMMIT;
-- Cast before aggregate: avoid an accidental integer SUM overflow on original int64 edge values.
SELECT count(*),sum(CAST({first} AS REAL)),avg(CAST({first} AS REAL)),sum(length({middle})),sum(length({last})) FROM wide_{width};
SELECT {','.join(columns)} FROM wide_{width} ORDER BY rowid;
PRAGMA integrity_check;
'''
        put(f'artifacts/generated/full/stress/wide-{width}-workload.sql', workload, kind='wide_workload_sql',
            planned_columns=width, explicit_projection_columns=width, planned_records_after=rows,
            future_max_column=width, prerequisite=f'artifacts/generated/full/wide-{width}.sql')

    boundary = []
    for maximum in profiles['future_max_column_builds']:
        for delta in profiles['boundary_offsets']:
            n = maximum + delta
            for family in ('table', 'view', 'select', 'index', 'update-set', 'group-by', 'order-by', 'insert-values'):
                c = ','.join(f'c{i}' for i in range(n))
                select = ','.join(f'{i} AS c{i}' for i in range(n))
                setup = ''
                if family == 'table':
                    body = 'CREATE TABLE boundary(' + c + ');'
                elif family == 'view':
                    # SQLite may defer view expansion; SELECT forces shape resolution.
                    body = 'CREATE VIEW boundary AS SELECT ' + select + ';\nSELECT * FROM boundary;'
                elif family == 'select':
                    body = 'SELECT ' + select + ';'
                elif family == 'index':
                    setup = 'CREATE TABLE boundary(c0 INTEGER);\n'
                    body = 'CREATE INDEX boundary_idx ON boundary(' + ','.join('c0' for _ in range(n)) + ');'
                elif family == 'update-set':
                    setup = 'CREATE TABLE boundary(c0 INTEGER); INSERT INTO boundary VALUES(1);\n'
                    body = 'UPDATE boundary SET ' + ','.join(f'c0={i}' for i in range(n)) + ';'
                elif family in ('group-by', 'order-by'):
                    setup = 'CREATE TABLE boundary(c0 INTEGER); INSERT INTO boundary VALUES(1);\n'
                    clause = 'GROUP BY' if family == 'group-by' else 'ORDER BY'
                    body = 'SELECT c0 FROM boundary ' + clause + ' ' + ','.join('c0' for _ in range(n)) + ';'
                else:
                    # Above-limit case uses a valid M-column table; its M+1 values
                    # also have a shape mismatch. Do not claim a unique error cause.
                    table_cols = min(n, maximum)
                    setup = 'CREATE TABLE boundary(' + ','.join(f'c{i}' for i in range(table_cols)) + ');\n'
                    body = 'INSERT INTO boundary VALUES(' + ','.join(str(i) for i in range(n)) + ');'
                relative = f'artifacts/generated/full/boundary/max-{maximum}/{family}-{n}.sql'
                expectation = 'success expected; unverified' if n <= maximum else 'error expected; unverified code/stage'
                put(relative, f'-- Future C MAX_COLUMN={maximum}; n={n}; {expectation}; NOT_RUN.\n' + setup + body + '\n',
                    kind='boundary_sql', family=family, future_max_column=maximum, requested_width=n,
                    limit_delta=delta, expectation=expectation)
                boundary.append({'id': f'max-{maximum}-{family}-{n}', 'sql_path': relative,
                                 'family': family, 'future_max_column': maximum, 'requested_width': n,
                                 'expectation': expectation, 'execution_status': 'NOT_RUN',
                                 'caveat': 'INSERT above-limit also has value/table arity mismatch' if family == 'insert-values' and delta > 0 else None})

    extra_path = ROOT/'extra-scope.json'
    extra_bytes = extra_path.read_bytes()
    extra = json.loads(extra_bytes)
    for relative in extra['static_source_paths']:
        put(relative,(ROOT/relative).read_text(),kind='additional_static_source',
            case_ids=[case['id'] for case in extra['cases'] if relative in case['paths']])
    put('extra-scope.json',extra_bytes.decode('utf-8'),kind='additional_scope')
    family_counts = {}
    for case in scenarios:
        family_counts[case['family']] = family_counts.get(case['family'], 0) + 1
    matrix = {'schema': 1, 'kind': 'STATIC_SCOPE_MATRIX', 'execution_status': 'NOT_RUN',
              'families': [{'family': key, 'static_scenarios': count, 'execution_status': 'NOT_RUN'} for key, count in sorted(family_counts.items())],
              'physical_data': {'normal_records':profiles['normal_records'], 'relational_records':relations},
              'wide_sql_plans': [{'columns':x['columns'],'planned_rows':x['planned_rows'],'execution_status':'NOT_RUN'} for x in profiles['wide_tables']],
              'large_relational_queries': {'path':'examples/large-join-queries.sql','planned_top_level_selects':13,'execution_status':'NOT_RUN'},
              'boundary_families': ['table','view','select','index','update-set','group-by','order-by','insert-values'],
              'additional_cases':extra['cases'],
              'execution_not_implemented': ['native C runner','Rust adapter','ACK/process orchestrator','API/fault runner'],
              'coverage_limit': 'Family examples only; not exhaustive grammar/combinations; no SQL validity or result proof.'}
    put('scope-matrix.json', json.dumps(matrix, ensure_ascii=False, indent=2) + '\n', kind='scope_matrix')
    manifest = {'schema': 1, 'kind': 'STATIC_GENERATION_MANIFEST', 'generator_version': profiles['generator_version'],
                'seed': profiles['seed'], 'profiles_sha256': sha((ROOT/'profiles.json').read_bytes()),
                'generator_sha256': sha(Path(__file__).read_bytes()), 'execution_status': 'NOT_RUN',
                'extra_scope_sha256': sha(extra_bytes), 'additional_cases':extra['cases'],
                'generation_scope': 'Physical text/data files only. No SQL engine, test, build or benchmark executed.',
                'normal_physical_records': profiles['normal_records'], 'readable_scenarios': len(scenarios),
                'relational_physical_records': relational_stats,
                'intentionally_negative_scenarios': sum(x.get('intentionally_negative',False) for x in scenarios),
                'boundary_files': len(boundary), 'files': entries, 'scenarios': scenarios, 'boundaries': boundary,
                'csv_conventions': {'null': '\\N', 'payload_hex': 'lowercase hex, empty string is empty BLOB',
                                    'text': 'UTF-8; CSV doubles backslash; uses backslash+0 for NUL, backslash+n for newline, backslash+r for CR; JSONL uses JSON escapes'},
                'unverified': ['SQL syntax and engine capability','all expected results/errors','runtime limits','wide inserted/materialized rows','SQL/C/Rust equivalence']}
    guarded_write(ROOT/'corpus-manifest.json', (json.dumps(manifest, ensure_ascii=False, indent=2) + '\n').encode('utf-8'))
    print(f'Static files generated: {len(entries)}; readable scenarios: {len(scenarios)}; boundary files: {len(boundary)}; normal records: {profiles["normal_records"]}. All SQL outcomes NOT_RUN.')


if __name__ == '__main__':
    main()
