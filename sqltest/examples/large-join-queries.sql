-- Static queries for artifacts/generated/full/relational.sql in its own future DB. NOT_RUN.
-- 01 Full materialization (all fields); no rowcount-only substitute.
SELECT id,name,city FROM customers ORDER BY id;
SELECT id,customer_id,total,note FROM orders ORDER BY id;
SELECT id,order_id,sku,qty,price FROM items ORDER BY id;
SELECT id,manager_id,name,dept FROM employees ORDER BY id;
SELECT id,device_id,at_sec,value,note FROM measurements ORDER BY id;
-- 02 Duplicate/NULL/unmatched relationship keys.
SELECT c.id,o.id,o.total FROM customers c FULL JOIN orders o ON o.customer_id=c.id ORDER BY c.id,o.id;
SELECT o.id,i.id,i.sku FROM orders o LEFT JOIN items i ON i.order_id=o.id ORDER BY o.id,i.id;
SELECT c.id,count(DISTINCT o.id),count(i.id),sum(i.qty*i.price) FROM customers c LEFT JOIN orders o ON o.customer_id=c.id LEFT JOIN items i ON i.order_id=o.id GROUP BY c.id ORDER BY c.id;
SELECT c.id FROM customers c WHERE EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;
SELECT c.id FROM customers c WHERE NOT EXISTS(SELECT 1 FROM orders o WHERE o.customer_id=c.id) ORDER BY c.id;
-- 03 Acyclic manager graph: parent id is strictly lower than child id.
WITH RECURSIVE tree(id,manager_id,name,depth) AS(SELECT id,manager_id,name,0 FROM employees WHERE manager_id IS NULL UNION ALL SELECT e.id,e.manager_id,e.name,t.depth+1 FROM employees e JOIN tree t ON e.manager_id=t.id) SELECT * FROM tree ORDER BY id;
-- 04 Tied timestamps and deterministic time-series frame.
SELECT id,device_id,at_sec,value,row_number() OVER(PARTITION BY device_id ORDER BY at_sec,id),avg(value) OVER(PARTITION BY device_id ORDER BY at_sec,id ROWS BETWEEN 5 PRECEDING AND CURRENT ROW),lag(value) OVER(PARTITION BY device_id ORDER BY at_sec,id) FROM measurements ORDER BY id;
SELECT device_id,(at_sec/60)*60 AS minute,count(*),min(value),max(value),avg(value) FROM measurements GROUP BY device_id,minute ORDER BY device_id,minute;
