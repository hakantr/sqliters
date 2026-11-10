-- Reader still holds snapshot; bounded busy wait. NOT_RUN.
PRAGMA busy_timeout=50;
PRAGMA wal_checkpoint(RESTART);
