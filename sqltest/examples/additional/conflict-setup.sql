-- Fresh DB per FAIL/ABORT/ROLLBACK recipe. NOT_RUN.
CREATE TABLE conflict_demo(id INTEGER PRIMARY KEY,v TEXT UNIQUE);
INSERT INTO conflict_demo VALUES(1,'a'),(2,'b');
BEGIN;
INSERT INTO conflict_demo VALUES(9,'pre');
