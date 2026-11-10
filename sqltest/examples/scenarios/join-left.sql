-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,o.id FROM customers c LEFT JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;
