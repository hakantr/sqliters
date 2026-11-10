-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE INDEX order_customer ON orders(customer_id); ANALYZE; REINDEX order_customer; SELECT count(*) FROM sqlite_stat1;
