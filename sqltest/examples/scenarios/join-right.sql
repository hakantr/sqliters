-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,o.id FROM customers c RIGHT JOIN orders o ON o.customer_id=c.id ORDER BY o.id,c.id;
