-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT customer_id AS k FROM orders INTERSECT SELECT k FROM join_keys ORDER BY k;
