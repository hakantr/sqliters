-- Future per-connection preflight AFTER WAL setup, BEFORE transactions. NOT_RUN.
PRAGMA synchronous=FULL;
PRAGMA wal_autocheckpoint=0;
PRAGMA busy_timeout=50;
PRAGMA journal_mode;
PRAGMA synchronous;
PRAGMA wal_autocheckpoint;
PRAGMA busy_timeout;
