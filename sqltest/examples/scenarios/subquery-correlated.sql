-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,(SELECT count(*) FROM orders o WHERE o.customer_id=c.id) AS n FROM customers c ORDER BY c.id;
