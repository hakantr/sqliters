-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT id,customer_id,row_number() OVER(PARTITION BY customer_id ORDER BY total,id),dense_rank() OVER(ORDER BY total) FROM orders ORDER BY id;
