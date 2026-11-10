-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
WITH RECURSIVE x(n) AS(VALUES(1) UNION ALL SELECT n+1 FROM x WHERE n<25) SELECT n,n*n FROM x ORDER BY n;
