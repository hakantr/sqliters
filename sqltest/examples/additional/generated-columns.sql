-- Generated VIRTUAL/STORED columns; fresh DB. NOT_RUN.
CREATE TABLE generated_demo(id INTEGER PRIMARY KEY,a INTEGER,b INTEGER GENERATED ALWAYS AS(a*2) VIRTUAL,c TEXT GENERATED ALWAYS AS(printf('%d',a)) STORED);
INSERT INTO generated_demo(id,a) VALUES(1,7),(2,NULL),(3,-9);
SELECT id,a,b,c,typeof(b),typeof(c) FROM generated_demo ORDER BY id;
UPDATE generated_demo SET a=a+1 WHERE id=1;
SELECT id,a,b,c FROM generated_demo ORDER BY id;
