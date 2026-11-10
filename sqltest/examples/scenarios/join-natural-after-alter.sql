-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE natural_copy(id INTEGER,nick TEXT); INSERT INTO natural_copy VALUES(1,'x'); ALTER TABLE natural_copy ADD COLUMN city TEXT; SELECT id,name,nick FROM customers NATURAL LEFT JOIN natural_copy ORDER BY id;
