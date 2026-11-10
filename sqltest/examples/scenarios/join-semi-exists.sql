-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id FROM customers c WHERE EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;
