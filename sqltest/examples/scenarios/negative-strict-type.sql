-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE strict_negative(v INTEGER) STRICT; INSERT INTO strict_negative VALUES('not-an-integer');
