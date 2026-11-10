-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TRIGGER inv_before BEFORE INSERT ON inventory WHEN new.qty<0 BEGIN SELECT RAISE(IGNORE); END; INSERT INTO inventory VALUES('N',-1,0); SELECT * FROM inventory ORDER BY sku;
