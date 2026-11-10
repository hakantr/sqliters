-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
UPDATE inventory SET qty=qty+1 RETURNING sku,qty; DELETE FROM inventory WHERE sku='B' RETURNING sku;
