-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE overflow_data(id INTEGER PRIMARY KEY,t TEXT,b BLOB); INSERT INTO overflow_data VALUES(1,replace(hex(zeroblob(9000)),'00','x'),zeroblob(20000)); SELECT id,length(t),length(b),hex(substr(b,19998,3)) FROM overflow_data; SELECT t,b FROM overflow_data ORDER BY id; PRAGMA integrity_check;
