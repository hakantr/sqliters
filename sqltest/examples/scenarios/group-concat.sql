-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT group_concat(sku,'|') FROM(SELECT sku FROM items ORDER BY id);
