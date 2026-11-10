-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
BEGIN DEFERRED; UPDATE inventory SET qty=qty+1; END TRANSACTION; SELECT * FROM inventory ORDER BY sku;
