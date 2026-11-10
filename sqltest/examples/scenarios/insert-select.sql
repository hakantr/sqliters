-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE selected AS SELECT id,name FROM customers WHERE id<=2; INSERT INTO selected SELECT id,name FROM customers WHERE id=3; SELECT * FROM selected ORDER BY id;
