-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
BEGIN IMMEDIATE; UPDATE inventory SET qty=qty+100; ROLLBACK; SELECT * FROM inventory ORDER BY sku;
