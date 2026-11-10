-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT date('2000-02-28','+1 day'),strftime('%Y-%m-%d','2000-01-01','+1 month'),unixepoch('1970-01-01 00:00:00');
