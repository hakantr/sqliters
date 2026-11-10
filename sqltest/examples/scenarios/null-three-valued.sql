-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT NULL=NULL,NULL IS NULL,1 IN(2,NULL),1 NOT IN(2,NULL),coalesce(NULL,7),CASE WHEN NULL THEN 1 ELSE 2 END;
