-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id FROM customers c WHERE c.id NOT IN(SELECT customer_id FROM orders) ORDER BY c.id;
