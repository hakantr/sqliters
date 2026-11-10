-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
REPLACE INTO inventory VALUES('A',99,5.0); SELECT * FROM inventory ORDER BY sku;
