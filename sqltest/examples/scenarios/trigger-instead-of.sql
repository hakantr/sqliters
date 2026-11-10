-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE VIEW inv_view AS SELECT sku,qty,price FROM inventory; CREATE TRIGGER view_ins INSTEAD OF INSERT ON inv_view BEGIN INSERT INTO inventory VALUES(new.sku,new.qty,new.price); END; INSERT INTO inv_view VALUES('C',4,2.0); SELECT * FROM inventory ORDER BY sku;
