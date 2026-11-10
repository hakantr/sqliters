-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE VIEW customer_totals AS SELECT customer_id,sum(total) AS total FROM orders GROUP BY customer_id; SELECT * FROM customer_totals ORDER BY customer_id; DROP VIEW customer_totals;
