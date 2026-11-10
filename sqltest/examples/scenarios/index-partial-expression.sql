-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE INDEX names_nonnull ON customers(lower(name)) WHERE city IS NOT NULL; SELECT name FROM customers WHERE city IS NOT NULL AND lower(name)='ada';
