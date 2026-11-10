-- Requires real close/reopen after page-65536-create.sql. NOT_RUN.
PRAGMA page_size;
PRAGMA encoding;
SELECT id,t,b FROM page_probe ORDER BY id;
PRAGMA integrity_check;
