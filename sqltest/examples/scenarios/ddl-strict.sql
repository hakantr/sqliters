-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE typed(a INTEGER,b TEXT) STRICT; INSERT INTO typed VALUES(1,'x'); SELECT typeof(a),typeof(b) FROM typed;
