-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
ATTACH DATABASE ':memory:' AS aux; CREATE TABLE aux.t(v INTEGER); INSERT INTO aux.t VALUES(9); SELECT * FROM aux.t; DETACH DATABASE aux;
