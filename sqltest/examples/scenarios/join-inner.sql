-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,c.name,o.id,o.total FROM customers c INNER JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;
