-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
-- Future execution only from sqltest cwd; destination must not exist.
VACUUM INTO 'artifacts/future-vacuum-output.sqlite';
