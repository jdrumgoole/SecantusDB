# reference-version: 15
# pgcrypto's ciphers: the raw encrypt()/decrypt() family and PGP over a
# password. PostgreSQL 15, whose pgcrypto (OpenSSL 3) refuses a bad pad and a
# ragged pad:none input where 14's accepted them.
CREATE EXTENSION pgcrypto
# Raw ciphers: deterministic, so compared byte for byte. The key is cut and
# zero-padded to the size the cipher picks; the IV to one block.
SELECT encode(encrypt('hello world'::bytea, 'key'::bytea, 'aes'), 'hex')
SELECT encode(encrypt('hello world'::bytea, '0123456789abcdef'::bytea, 'aes-cbc/pad:pkcs'), 'hex')
SELECT encode(encrypt('0123456789abcdef'::bytea, 'k'::bytea, 'aes-ecb/pad:none'), 'hex')
SELECT encode(encrypt('hello world'::bytea, repeat('k', 20)::bytea, 'aes'), 'hex'), encode(encrypt('hello world'::bytea, repeat('k', 40)::bytea, 'aes'), 'hex')
SELECT encode(encrypt('abc'::bytea, ''::bytea, 'AES'), 'hex'), encode(encrypt('abc'::bytea, 'k'::bytea, 'rijndael'), 'hex')
SELECT encode(encrypt_iv('hello world'::bytea, 'k'::bytea, 'iv12345678901234'::bytea, 'aes-cbc'), 'hex'), encode(encrypt_iv('abc'::bytea, 'k'::bytea, repeat('i', 40)::bytea, 'aes'), 'hex')
SELECT encode(encrypt('hello world'::bytea, 'key'::bytea, '3des'), 'hex'), encode(encrypt('abcdefghijklmnopq'::bytea, 'k'::bytea, '3des-ecb'), 'hex')
SELECT convert_from(decrypt(encrypt('hello'::bytea, 'k'::bytea, 'aes'), 'k'::bytea, 'aes'), 'UTF8'), convert_from(decrypt_iv(encrypt_iv('hi'::bytea, 'k'::bytea, 'iv'::bytea, 'aes'), 'k'::bytea, 'iv'::bytea, 'aes'), 'UTF8')
SELECT encode(decrypt('\x00112233445566778899aabbccddeeff'::bytea, 'k'::bytea, 'aes/pad:none'), 'hex')
SELECT decrypt('\x00112233445566778899aabbccddeeff'::bytea, 'k'::bytea, 'aes')
SELECT decrypt('\x0102'::bytea, 'k'::bytea, 'aes')
SELECT encrypt('abc'::bytea, 'k'::bytea, 'aes/pad:none')
SELECT encrypt('abc'::bytea, 'k'::bytea, 'aes-cfb')
SELECT encrypt('abc'::bytea, 'k'::bytea, 'aes/pad:zero')
SELECT encrypt('abc'::bytea, 'k'::bytea, 'aes/foo:bar')
SELECT encrypt(NULL, 'k'::bytea, 'aes'), pg_typeof(encrypt('a', 'k', 'aes'))
# PGP over a password. Ciphertext is random, so a line checks a round trip
# or a property; cross-DECRYPTION against PostgreSQL is a separate harness.
SELECT pgp_sym_decrypt(pgp_sym_encrypt('secret', 'pw'), 'pw')
SELECT pgp_sym_decrypt(pgp_sym_encrypt('secret', 'pw', 'cipher-algo=aes256, compress-algo=2'), 'pw')
SELECT pgp_sym_decrypt(pgp_sym_encrypt('a' || chr(10) || 'b', 'pw', 'convert-crlf=1'), 'pw', 'convert-crlf=1') = 'a' || chr(10) || 'b'
SELECT encode(pgp_sym_decrypt_bytea(pgp_sym_encrypt_bytea('\x00ff'::bytea, 'pw', 'disable-mdc=1, sess-key=1'), 'pw'), 'hex')
SELECT pgp_sym_decrypt(pgp_sym_encrypt('secret', 'pw'), 'wrong')
SELECT pgp_sym_decrypt(pgp_sym_encrypt_bytea('\x00'::bytea, 'pw'), 'pw')
SELECT pgp_sym_encrypt('x', 'pw', 'bogus=1')
SELECT pgp_sym_encrypt('x', 'pw', 'cipher-algo=nope')
SELECT pgp_sym_encrypt('x', 'pw', 'compress-algo=7')
SELECT pgp_sym_decrypt('\x0102'::bytea, 'pw')
SELECT pgp_key_id(pgp_sym_encrypt('x', 'pw')), pg_typeof(pgp_sym_encrypt('x', 'pw'))
SELECT armor('\x010203'::bytea), armor('abc'::bytea, ARRAY['Comment', 'Version'], ARRAY['hi', 'x'])
SELECT encode(dearmor(armor('\x00ff10'::bytea)), 'hex')
SELECT dearmor('not armor')
SELECT dearmor('-----BEGIN PGP MESSAGE-----' || chr(10) || chr(10) || 'YWJj' || chr(10) || '=AAAA' || chr(10) || '-----END PGP MESSAGE-----' || chr(10))
# PGP over a KEY. The keys (pcx_keys) are OpenPGP v4 key data PostgreSQL
# accepted: an RSA primary with an RSA subkey (plain and password-protected),
# DSA with an Elgamal subkey, two primaries, and two encryption subkeys.
SELECT name, pgp_key_id(pub), pgp_key_id(sec) FROM pcx_keys WHERE name IN ('rsa', 'rsa_pw', 'elg') ORDER BY name
SELECT pgp_key_id(pub) FROM pcx_keys WHERE name = 'two'
SELECT pgp_key_id(sec) FROM pcx_keys WHERE name = 'twosub'
SELECT name, pgp_pub_decrypt(pgp_pub_encrypt('secret ünï', pub), sec, 'keypw') FROM pcx_keys WHERE name IN ('rsa', 'rsa_pw', 'elg') ORDER BY name
SELECT pgp_pub_decrypt(pgp_pub_encrypt('hi', pub, 'cipher-algo=aes256, compress-algo=2, disable-mdc=1'), sec) FROM pcx_keys WHERE name = 'elg'
SELECT encode(pgp_pub_decrypt_bytea(pgp_pub_encrypt_bytea('\x00ff'::bytea, pub), sec), 'hex') FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_key_id(pgp_pub_encrypt('x', pub)) = pgp_key_id(pub) FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_decrypt(pgp_pub_encrypt('x', pub), sec) FROM pcx_keys WHERE name = 'rsa_pw'
SELECT pgp_pub_decrypt(pgp_pub_encrypt('x', pub), sec, 'nope') FROM pcx_keys WHERE name = 'rsa_pw'
SELECT pgp_pub_decrypt(pgp_pub_encrypt('x', pub), pub) FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_encrypt('x', sec) FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_encrypt('x', pub) FROM pcx_keys WHERE name = 'two'
SELECT pgp_pub_encrypt('x', pub) FROM pcx_keys WHERE name = 'twosub'
SELECT pgp_pub_decrypt(pgp_pub_encrypt('x', a.pub), b.sec) FROM pcx_keys a, pcx_keys b WHERE a.name = 'rsa' AND b.name = 'elg'
SELECT pgp_sym_decrypt(pgp_pub_encrypt('x', pub), 'k') FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_decrypt(pgp_sym_encrypt('x', 'k'), sec) FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_decrypt(pgp_pub_encrypt_bytea('\x00'::bytea, pub), sec) FROM pcx_keys WHERE name = 'rsa'
SELECT pgp_pub_encrypt('x', '\x0102'::bytea)
DROP TABLE pcx_keys
DROP EXTENSION pgcrypto
