INSERT INTO ex_b VALUES (1, 101, '[2020-01-01 10:00, 2020-01-01 12:00)')
INSERT INTO ex_b VALUES (2, 101, '[2020-01-01 11:00, 2020-01-01 13:00)')
INSERT INTO ex_b VALUES (3, 102, '[2020-01-01 11:00, 2020-01-01 13:00)')
INSERT INTO ex_b VALUES (4, 101, '[2020-01-01 12:00, 2020-01-01 13:00)')
UPDATE ex_b SET during = '[2020-01-01 09:00, 2020-01-01 12:30)' WHERE id = 4
SELECT id, room FROM ex_b ORDER BY id
INSERT INTO ex_e VALUES (1)
INSERT INTO ex_e VALUES (1)
INSERT INTO ex_e VALUES (NULL), (NULL)
INSERT INTO ex_r VALUES (1, '[1,5)'), (2, '[5,9)')
INSERT INTO ex_r VALUES (3, '[4,6)')
INSERT INTO ex_r VALUES (4, '[10,12)'), (5, '[11,13)')
SELECT conname, contype, pg_get_constraintdef(oid) FROM pg_constraint WHERE conrelid = 'ex_r'::regclass ORDER BY conname
SELECT conname, contype FROM pg_constraint WHERE conrelid = 'ex_b'::regclass ORDER BY conname
