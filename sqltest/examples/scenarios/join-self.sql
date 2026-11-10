-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT e.name,m.name AS manager FROM employees e LEFT JOIN employees m ON e.manager_id=m.id ORDER BY e.id;
