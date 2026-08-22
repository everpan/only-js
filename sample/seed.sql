CREATE TABLE IF NOT EXISTS account (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, role TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS orders (id INTEGER PRIMARY KEY AUTOINCREMENT, no TEXT NOT NULL, account_id INTEGER NOT NULL, amount REAL NOT NULL);
INSERT OR IGNORE INTO account (id, name, role) VALUES (1, 'neo', 'admin');
INSERT OR IGNORE INTO account (id, name, role) VALUES (2, 'trinity', 'user');
INSERT OR IGNORE INTO orders (id, no, account_id, amount) VALUES (1, 'A-0001', 1, 99.5);
INSERT OR IGNORE INTO orders (id, no, account_id, amount) VALUES (2, 'A-0002', 2, 0.5);
