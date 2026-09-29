SELECT id FROM wf_t WHERE lower(email) = 'a@x.com' ORDER BY id
SELECT count(*) FROM wf_t WHERE lower(email) = 'a@x.com'
SELECT count(*) FROM wf_t WHERE upper(email) LIKE 'A%'
SELECT sum(n) FROM wf_t WHERE length(email) > 1
SELECT email, count(*) FROM wf_t WHERE abs(n - 2) <= 1 GROUP BY email ORDER BY email
SELECT max(n) FROM wf_t WHERE n % 2 = 1
