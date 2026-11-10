-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
UPDATE inventory SET qty=qty+2,price=price*1.125 WHERE sku='A'; DELETE FROM inventory WHERE sku='B'; SELECT * FROM inventory ORDER BY sku;
