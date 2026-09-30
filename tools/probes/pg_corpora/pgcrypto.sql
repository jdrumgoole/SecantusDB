SELECT digest('abc', 'sha256')
CREATE EXTENSION pgcrypto
SELECT encode(digest('abc', 'sha256'), 'hex')
SELECT encode(digest('abc', 'md5'), 'hex'), encode(digest('abc', 'sha1'), 'hex'), encode(digest('abc', 'sha512'), 'hex')
SELECT encode(digest('\x616263'::bytea, 'sha224'), 'hex')
SELECT digest('abc', 'nope')
SELECT encode(hmac('data', 'key', 'sha256'), 'hex'), encode(hmac('data', 'key', 'md5'), 'hex')
SELECT length(gen_random_bytes(16)), gen_random_bytes(0)
SELECT gen_random_bytes(2000)
SELECT crypt('secret', '$1$abcdefgh')
SELECT crypt('secret', '$2a$06$abcdefghijklmnopqrstuu')
SELECT crypt('secret', '_J9..abcd')
SELECT crypt('secret', 'ab')
SELECT substr(gen_salt('bf'), 1, 7), length(gen_salt('bf')), substr(gen_salt('bf', 8), 1, 7)
SELECT substr(gen_salt('md5'), 1, 3), length(gen_salt('md5')), length(gen_salt('xdes')), length(gen_salt('des'))
SELECT gen_salt('bf', 3)
SELECT gen_salt('nope')
SELECT crypt('pw', s) = crypt('pw', crypt('pw', s)) FROM (SELECT gen_salt('bf') AS s) x
SELECT crypt('pw', s) = crypt('other', s) FROM (SELECT gen_salt('md5') AS s) x
DROP EXTENSION pgcrypto
SELECT gen_salt('bf')
