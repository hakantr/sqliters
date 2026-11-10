-- Composite FK and WITHOUT ROWID; fresh DB. NOT_RUN.
PRAGMA foreign_keys=ON;
CREATE TABLE wr_parent(k TEXT,n INTEGER,label TEXT,PRIMARY KEY(k,n)) WITHOUT ROWID;
CREATE TABLE wr_child(k TEXT,n INTEGER,seq INTEGER,PRIMARY KEY(k,n,seq),FOREIGN KEY(k,n) REFERENCES wr_parent(k,n) ON UPDATE CASCADE ON DELETE CASCADE) WITHOUT ROWID;
INSERT INTO wr_parent VALUES('東京',1,'p'),('Ada',2,'q');
INSERT INTO wr_child VALUES('東京',1,1),('東京',1,2),('Ada',2,1);
UPDATE wr_parent SET n=3 WHERE k='東京';
SELECT * FROM wr_child ORDER BY k,n,seq;
DELETE FROM wr_parent WHERE k='Ada';
SELECT * FROM wr_child ORDER BY k,n,seq;
PRAGMA foreign_key_check;
