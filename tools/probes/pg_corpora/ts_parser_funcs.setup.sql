DROP TABLE IF EXISTS tspf_aliases CASCADE
DROP TABLE IF EXISTS tspf_ord CASCADE
DROP TABLE IF EXISTS tspf_txt CASCADE
DROP TABLE IF EXISTS tspf_docs CASCADE
CREATE TABLE tspf_aliases (t tsquery, s tsquery)
INSERT INTO tspf_aliases VALUES ('supernovae', 'supernovae | sn'), ('sn', 'supernovae'), ('a & b', 'c'), ('x', NULL), (NULL, 'y'), ('', 'z')
CREATE TABLE tspf_ord (n int, t tsquery, s tsquery)
INSERT INTO tspf_ord VALUES (1, 'a', 'b'), (2, 'b', 'c'), (3, 'c', 'd')
CREATE TABLE tspf_txt (t text, s text)
INSERT INTO tspf_txt VALUES ('a', 'b')
CREATE TABLE tspf_docs (id int, body text)
INSERT INTO tspf_docs VALUES (1, 'The quick-brown fox at http://example.com/a.html'), (2, 'mail me@example.org v1.2.3 -5 3.14 1e-3'), (3, '<p class="x">Ünïcödé naïve</p> &amp; a-1-b')
