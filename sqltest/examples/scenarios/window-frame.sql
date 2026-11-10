-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT id,sum(total) OVER(ORDER BY id ROWS BETWEEN 1 PRECEDING AND CURRENT ROW),lag(total) OVER(ORDER BY id),lead(total) OVER(ORDER BY id) FROM orders ORDER BY id;
