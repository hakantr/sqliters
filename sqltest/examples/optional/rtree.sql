-- Optional SQLITE_ENABLE_RTREE; NOT_RUN.
CREATE VIRTUAL TABLE boxes USING rtree(id,min_x,max_x,min_y,max_y);
INSERT INTO boxes VALUES(1,0,10,0,10),(2,-5,1,-5,1),(3,20,30,20,30),(4,0.1,0.2,0.1,0.2);
SELECT id,min_x,max_x,min_y,max_y FROM boxes WHERE min_x<=1 AND max_x>=0 AND min_y<=1 AND max_y>=0 ORDER BY id;
