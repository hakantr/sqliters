-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
BEGIN; SAVEPOINT pending; INSERT INTO fk_child VALUES(20,99); ROLLBACK TO pending; RELEASE pending; COMMIT; SELECT * FROM fk_child ORDER BY id;
