-- Writer connection stays open throughout schedule. Fresh sandbox DB; NOT_RUN.
PRAGMA journal_mode=WAL;
PRAGMA synchronous=FULL;
PRAGMA wal_autocheckpoint=0;
PRAGMA busy_timeout=50;
CREATE TABLE ledger(id INTEGER PRIMARY KEY,balance INTEGER);
INSERT INTO ledger VALUES(1,100),(2,200);
SELECT sum(balance) FROM ledger;
