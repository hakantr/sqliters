-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
DELETE FROM fk_parent WHERE id=1; SELECT * FROM fk_child ORDER BY id;
