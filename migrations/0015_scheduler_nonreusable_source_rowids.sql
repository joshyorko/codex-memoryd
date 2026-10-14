-- Stable scheduler row IDs decouple saved cursors from SQLite rowid reuse.
CREATE TABLE IF NOT EXISTS scheduler_source_rowid_counter (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    sequence INTEGER NOT NULL CHECK (sequence >= 0)
);

INSERT OR IGNORE INTO scheduler_source_rowid_counter(singleton, sequence)
VALUES (1, 0);

CREATE TABLE IF NOT EXISTS scheduler_source_rowids (
    stream TEXT NOT NULL,
    source_key TEXT NOT NULL,
    scheduler_rowid INTEGER NOT NULL CHECK (scheduler_rowid > 0),
    PRIMARY KEY (stream, source_key),
    UNIQUE (stream, scheduler_rowid)
);

INSERT OR IGNORE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
SELECT 'visible_turns', id, rowid FROM visible_turns;
INSERT OR IGNORE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
SELECT 'conclusions', id, rowid FROM conclusions;
INSERT OR IGNORE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
SELECT 'checkpoints', id, rowid FROM checkpoints;
INSERT OR IGNORE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
SELECT 'memory_sources', id, rowid FROM memory_sources;
INSERT OR IGNORE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
SELECT 'memory_records', id, rowid FROM memory_records;

UPDATE scheduler_source_rowid_counter
SET sequence = MAX(
    sequence,
    COALESCE((SELECT MAX(scheduler_rowid) FROM scheduler_source_rowids), 0)
)
WHERE singleton = 1;

CREATE INDEX IF NOT EXISTS idx_scheduler_source_rowids_rowid
    ON scheduler_source_rowids(stream, scheduler_rowid);

CREATE TRIGGER IF NOT EXISTS visible_turns_scheduler_rowid_insert
AFTER INSERT ON visible_turns BEGIN
    UPDATE scheduler_source_rowid_counter SET sequence = sequence + 1 WHERE singleton = 1;
    INSERT OR REPLACE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
    SELECT 'visible_turns', NEW.id, sequence
    FROM scheduler_source_rowid_counter WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS visible_turns_scheduler_rowid_delete
AFTER DELETE ON visible_turns BEGIN
    DELETE FROM scheduler_source_rowids WHERE stream = 'visible_turns' AND source_key = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS conclusions_scheduler_rowid_insert
AFTER INSERT ON conclusions BEGIN
    UPDATE scheduler_source_rowid_counter SET sequence = sequence + 1 WHERE singleton = 1;
    INSERT OR REPLACE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
    SELECT 'conclusions', NEW.id, sequence
    FROM scheduler_source_rowid_counter WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS conclusions_scheduler_rowid_delete
AFTER DELETE ON conclusions BEGIN
    DELETE FROM scheduler_source_rowids WHERE stream = 'conclusions' AND source_key = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS checkpoints_scheduler_rowid_insert
AFTER INSERT ON checkpoints BEGIN
    UPDATE scheduler_source_rowid_counter SET sequence = sequence + 1 WHERE singleton = 1;
    INSERT OR REPLACE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
    SELECT 'checkpoints', NEW.id, sequence
    FROM scheduler_source_rowid_counter WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS checkpoints_scheduler_rowid_delete
AFTER DELETE ON checkpoints BEGIN
    DELETE FROM scheduler_source_rowids WHERE stream = 'checkpoints' AND source_key = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS memory_sources_scheduler_rowid_insert
AFTER INSERT ON memory_sources BEGIN
    UPDATE scheduler_source_rowid_counter SET sequence = sequence + 1 WHERE singleton = 1;
    INSERT OR REPLACE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
    SELECT 'memory_sources', NEW.id, sequence
    FROM scheduler_source_rowid_counter WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS memory_sources_scheduler_rowid_delete
AFTER DELETE ON memory_sources BEGIN
    DELETE FROM scheduler_source_rowids WHERE stream = 'memory_sources' AND source_key = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS memory_records_scheduler_rowid_insert
AFTER INSERT ON memory_records BEGIN
    UPDATE scheduler_source_rowid_counter SET sequence = sequence + 1 WHERE singleton = 1;
    INSERT OR REPLACE INTO scheduler_source_rowids(stream, source_key, scheduler_rowid)
    SELECT 'memory_records', NEW.id, sequence
    FROM scheduler_source_rowid_counter WHERE singleton = 1;
END;
CREATE TRIGGER IF NOT EXISTS memory_records_scheduler_rowid_delete
AFTER DELETE ON memory_records BEGIN
    DELETE FROM scheduler_source_rowids WHERE stream = 'memory_records' AND source_key = OLD.id;
END;
