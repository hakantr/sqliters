-- Static scenario; NOT_RUN. Fresh schema.sql + seed.sql unless stated.
SELECT c.id,o.id,i.sku FROM customers c JOIN orders o ON c.id=o.customer_id LEFT JOIN items i ON i.order_id=o.id ORDER BY c.id,o.id,i.id;
