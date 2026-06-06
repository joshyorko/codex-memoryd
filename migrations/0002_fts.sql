-- Optional FTS5 index over memory_records. Applied only when the SQLite build
-- reports FTS5 support; otherwise the store falls back to LIKE search and
-- reports degraded status (SPEC §6.1 storage features).

CREATE VIRTUAL TABLE IF NOT EXISTS memory_records_fts USING fts5(
    id UNINDEXED,
    content,
    tags,
    related_files,
    tokenize = 'porter unicode61'
);

-- Keep the FTS index in sync with the base table via triggers.
CREATE TRIGGER IF NOT EXISTS memory_records_ai AFTER INSERT ON memory_records BEGIN
    INSERT INTO memory_records_fts(id, content, tags, related_files)
    VALUES (new.id, new.content, new.tags, new.related_files);
END;

CREATE TRIGGER IF NOT EXISTS memory_records_ad AFTER DELETE ON memory_records BEGIN
    DELETE FROM memory_records_fts WHERE id = old.id;
END;

CREATE TRIGGER IF NOT EXISTS memory_records_au AFTER UPDATE ON memory_records BEGIN
    DELETE FROM memory_records_fts WHERE id = old.id;
    INSERT INTO memory_records_fts(id, content, tags, related_files)
    VALUES (new.id, new.content, new.tags, new.related_files);
END;
