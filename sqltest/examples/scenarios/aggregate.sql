-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT customer_id,count(*),count(note),sum(total),avg(total),min(total),max(total) FROM orders GROUP BY customer_id HAVING count(*)>0 ORDER BY customer_id;
