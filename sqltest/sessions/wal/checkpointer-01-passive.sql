-- Checkpointer role; in 2-connection mode this is writer alias. NOT_RUN.
PRAGMA busy_timeout=50;
PRAGMA wal_checkpoint(PASSIVE);
