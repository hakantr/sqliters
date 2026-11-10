-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT id FROM customers EXCEPT SELECT customer_id FROM orders ORDER BY id;
