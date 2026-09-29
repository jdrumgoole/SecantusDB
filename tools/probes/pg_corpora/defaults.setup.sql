DROP TABLE IF EXISTS dx
DROP SEQUENCE IF EXISTS dxs
CREATE SEQUENCE dxs START 100
CREATE TABLE dx (id int PRIMARY KEY DEFAULT nextval('dxs'), created timestamptz DEFAULT now(), day date DEFAULT current_date, u uuid DEFAULT gen_random_uuid(), n int DEFAULT 1 + 2, s text DEFAULT 'x' || 'y')
