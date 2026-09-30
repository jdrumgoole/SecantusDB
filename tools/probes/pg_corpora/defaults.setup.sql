DROP TABLE IF EXISTS dx
DROP SEQUENCE IF EXISTS dxs
CREATE SEQUENCE dxs START 100
CREATE TABLE dx (id int PRIMARY KEY DEFAULT nextval('dxs'), created timestamptz DEFAULT now(), day date DEFAULT current_date, u uuid DEFAULT gen_random_uuid(), n int DEFAULT 1 + 2, s text DEFAULT 'x' || 'y')
DROP TABLE IF EXISTS cdf
CREATE TABLE cdf (a int DEFAULT 1 + 2, b text DEFAULT 'x' || 'y', c int DEFAULT abs(-3), d numeric DEFAULT 1.5 * 2, f int DEFAULT -1, h boolean DEFAULT (NOT false), i int[] DEFAULT '{1,2}', l varchar DEFAULT 'v', m int DEFAULT -2 * 3, n boolean DEFAULT (true AND false), o numeric DEFAULT -1.5, p text[] DEFAULT '{a,b}', q float8 DEFAULT -2.5, r bigint DEFAULT -9, t bigint DEFAULT -9000000000, u numeric DEFAULT -3)
