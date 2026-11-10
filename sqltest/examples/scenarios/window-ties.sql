-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT k,label,rank() OVER(ORDER BY k),sum(k) OVER(ORDER BY k RANGE BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) FROM join_keys ORDER BY label;
