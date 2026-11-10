-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
INSERT INTO inventory VALUES('A',2,3.5) ON CONFLICT(sku) DO UPDATE SET qty=inventory.qty+excluded.qty,price=excluded.price RETURNING sku,qty,price;
