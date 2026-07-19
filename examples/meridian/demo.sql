-- Meridian's compact SQL surface keeps schemas explicit for deterministic tests.
CREATE TABLE teams 3;
COLUMN teams id INT REQUIRED UNIQUE;
COLUMN teams region INT REQUIRED;
COLUMN teams budget INT REQUIRED;
CREATE TABLE users 3;
COLUMN users id INT REQUIRED UNIQUE;
COLUMN users team INT REQUIRED;
COLUMN users score INT REQUIRED;
INSERT teams 1 10 1000;
INSERT teams 2 20 2000;
INSERT users 1 1 65;
INSERT users 2 1 82;
INSERT users 3 2 91;
CREATE INDEX users_score users score;
CREATE INDEX teams_id teams id UNIQUE;
BEGIN 7;
COMMIT 7;
SELECT users FILTER score 70 JOIN teams ON team id GROUP team ORDER score LIMIT 10;
