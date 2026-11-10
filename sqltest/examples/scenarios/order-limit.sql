-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT id,total FROM orders ORDER BY total DESC,id LIMIT 3 OFFSET 1;
