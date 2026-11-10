-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SAVEPOINT outer_sp; UPDATE inventory SET qty=qty+1; SAVEPOINT inner_sp; DELETE FROM inventory; ROLLBACK TO inner_sp; RELEASE inner_sp; RELEASE outer_sp; SELECT * FROM inventory ORDER BY sku;
