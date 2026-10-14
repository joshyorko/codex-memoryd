-- Commit-ordered mutation markers for scheduler-visible memory reactivation.
CREATE TABLE IF NOT EXISTS active_memory_mutation_counter (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    sequence INTEGER NOT NULL
);

INSERT OR IGNORE INTO active_memory_mutation_counter(singleton, sequence)
VALUES (1, 0);
