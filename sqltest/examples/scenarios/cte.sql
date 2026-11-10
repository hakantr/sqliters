-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
WITH totals AS(SELECT customer_id,sum(total) AS total FROM orders GROUP BY customer_id) SELECT c.id,t.total FROM customers c LEFT JOIN totals t ON t.customer_id=c.id ORDER BY c.id;
