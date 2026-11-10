-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
CREATE TABLE keys2(k INTEGER,v TEXT); INSERT INTO keys2 VALUES(1,'hit'),(NULL,'null'),(8,'miss'); SELECT k,label,v FROM join_keys LEFT JOIN keys2 USING(k) ORDER BY label,v;
