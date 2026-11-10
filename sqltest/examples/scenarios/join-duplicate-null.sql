-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT a.label,b.label FROM join_keys a FULL JOIN join_keys b ON a.k=b.k ORDER BY a.label,b.label;
