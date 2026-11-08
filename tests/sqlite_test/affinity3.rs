#[test]
#[ignore = "0/16 subtests passed; pending: VIEW"]
fn test_affinity3() {
    sqliters_tcltest::run_test_script(
        "affinity3",
        r#"
set testdir [file dirname $argv0]
source $testdir/tester.tcl
do_execsql_test affinity3-100 {
  CREATE TABLE customer (id INT PRIMARY KEY);
  CREATE TABLE apr (id INT PRIMARY KEY, apr REAL);
  
  CREATE VIEW v1 AS
  SELECT c.id, i.apr
  FROM customer c
  LEFT JOIN apr i ON i.id=c.id;
  
  CREATE VIEW v1rj AS
  SELECT c.id, i.apr
  FROM apr i
  RIGHT JOIN customer c ON i.id=c.id;
  
  CREATE VIEW v2 AS
  SELECT c.id, v1.apr
  FROM customer c
  LEFT JOIN v1 ON v1.id=c.id;
  
  CREATE VIEW v2rj AS
  SELECT c.id, v1.apr
  FROM v1 RIGHT JOIN customer c ON v1.id=c.id;
  
  CREATE VIEW v2rjrj AS
  SELECT c.id, v1rj.apr
  FROM v1rj RIGHT JOIN customer c ON v1rj.id=c.id;
  
  INSERT INTO customer (id) VALUES (1);
  INSERT INTO apr (id, apr) VALUES (1, 12);
  INSERT INTO customer (id) VALUES (2);
  INSERT INTO apr (id, apr) VALUES (2, 12.01);
}
do_execsql_test affinity3-110 {
  PRAGMA automatic_index=ON;
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v1;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-111 {
  PRAGMA automatic_index=ON;
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v1rj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-120 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-121 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2rj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-122 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2rjrj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-130 {
  PRAGMA automatic_index=OFF;
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v1;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-131 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v1rj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-140 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-141 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2rj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-142 {
  SELECT id, (apr / 100), typeof(apr) apr_type  FROM v2rjrj;
} {1 0.12 real 2 0.1201 real}
do_execsql_test affinity3-200 {
  CREATE TABLE map_integer (id INT, name);
  INSERT INTO map_integer VALUES(1,'a');
  CREATE TABLE map_text (id TEXT, name);
  INSERT INTO map_text VALUES('4','e');
  CREATE TABLE data (id TEXT, name);
  INSERT INTO data VALUES(1,'abc');
  INSERT INTO data VALUES('4','xyz');
  CREATE VIEW idmap as
      SELECT * FROM map_integer
      UNION SELECT * FROM map_text;
  CREATE TABLE mzed AS SELECT * FROM idmap;
}
do_execsql_test affinity3-210 {
  PRAGMA automatic_index=ON;
  SELECT * FROM data JOIN idmap USING(id);
} {4 xyz e}
do_execsql_test affinity3-220 {
  SELECT * FROM data JOIN mzed USING(id);
} {4 xyz e}
do_execsql_test affinity3-250 {
  PRAGMA automatic_index=OFF;
  SELECT * FROM data JOIN idmap USING(id);
} {4 xyz e}
do_execsql_test affinity3-260 {
  SELECT * FROM data JOIN mzed USING(id);
} {4 xyz e}
finish_test
"#,
    );
}
