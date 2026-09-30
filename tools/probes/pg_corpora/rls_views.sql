# Row-level security on a table read THROUGH A VIEW: the view owner's
# policies apply (and the owner's exemption), while current_user in a policy
# is still the caller.
SET ROLE rv_alice
SELECT * FROM rv_t ORDER BY id
SELECT * FROM rv_v ORDER BY id
SELECT * FROM rv_vb ORDER BY id
SELECT * FROM rv_vc ORDER BY id
SELECT count(*) FROM rv_v JOIN rv_t USING (id)
SELECT (SELECT count(*) FROM rv_vb), (SELECT count(*) FROM rv_t)
RESET ROLE
SET ROLE rv_bob
SELECT * FROM rv_t ORDER BY id
SELECT * FROM rv_v ORDER BY id
RESET ROLE
SELECT * FROM rv_vb ORDER BY id
