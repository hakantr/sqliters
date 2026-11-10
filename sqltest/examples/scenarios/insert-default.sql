-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE defaults(id INTEGER PRIMARY KEY,v TEXT DEFAULT 'd'); INSERT INTO defaults DEFAULT VALUES; SELECT * FROM defaults;
