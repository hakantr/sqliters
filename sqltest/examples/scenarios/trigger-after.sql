-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TRIGGER inv_after AFTER UPDATE ON inventory BEGIN INSERT INTO audit VALUES('update',new.qty); END; UPDATE inventory SET qty=qty+1 WHERE sku='A'; SELECT * FROM audit; DROP TRIGGER inv_after;
