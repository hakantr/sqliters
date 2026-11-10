-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE mutable(a INTEGER,b TEXT); ALTER TABLE mutable ADD COLUMN c BLOB; ALTER TABLE mutable RENAME COLUMN b TO label; ALTER TABLE mutable DROP COLUMN c; ALTER TABLE mutable RENAME TO renamed; PRAGMA table_info(renamed);
