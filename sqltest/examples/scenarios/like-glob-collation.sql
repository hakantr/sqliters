-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT name FROM customers WHERE name LIKE '%a%' OR name GLOB '*[A-Z]*' ORDER BY name COLLATE NOCASE,name COLLATE BINARY;
