-- Fresh future DB only; close/reopen is external API action, not SQL. NOT_RUN.
PRAGMA page_size=4096;
PRAGMA encoding='UTF-8';
PRAGMA journal_mode=DELETE;
CREATE TABLE page_probe(id INTEGER PRIMARY KEY,t TEXT,b BLOB);
WITH RECURSIVE seq(i) AS(VALUES(1) UNION ALL SELECT i+1 FROM seq WHERE i<256) INSERT INTO page_probe SELECT i,printf('row-%04d-東京',i),zeroblob(CASE WHEN i%7=0 THEN 9000 ELSE i%257 END) FROM seq;
