-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE composite(a INTEGER,b TEXT,PRIMARY KEY(a,b)) WITHOUT ROWID; INSERT INTO composite VALUES(1,'x'); SELECT * FROM composite; DROP TABLE composite;
