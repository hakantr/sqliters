-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE nick(id INTEGER,nick TEXT); INSERT INTO nick VALUES(1,'A'),(2,'B'),(8,'Z'); SELECT id,name,nick FROM customers NATURAL LEFT JOIN nick ORDER BY id;
