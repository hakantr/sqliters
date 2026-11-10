-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,j.k,j.label FROM customers c CROSS JOIN join_keys j ORDER BY c.id,j.label;
