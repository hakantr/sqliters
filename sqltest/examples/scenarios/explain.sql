-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
EXPLAIN SELECT id FROM orders WHERE customer_id=1; EXPLAIN QUERY PLAN SELECT c.id,o.id FROM customers c JOIN orders o ON c.id=o.customer_id;
