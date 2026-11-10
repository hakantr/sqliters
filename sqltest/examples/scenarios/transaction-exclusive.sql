-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
BEGIN EXCLUSIVE; SELECT count(*) FROM customers; COMMIT;
