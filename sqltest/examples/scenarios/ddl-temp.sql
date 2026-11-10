-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TEMP TABLE scratch(k INTEGER); INSERT INTO scratch VALUES(7); SELECT k FROM temp.scratch; DROP TABLE scratch;
