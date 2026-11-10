-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
INSERT OR IGNORE INTO inventory VALUES('A',1,0.0); SELECT * FROM inventory ORDER BY sku;
