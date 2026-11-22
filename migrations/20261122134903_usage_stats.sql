-- usage_stats: what each turn spent and did, and where usage limits stood,
-- for the dashboard. Rows outlive their threads: a deleted thread's turns
-- still count toward totals, so `thread` isn't a foreign key.

CREATE TABLE turns (
    id INTEGER PRIMARY KEY,
    thread INTEGER,
    -- The project's folder, or '' for regular chats, as in `projects.path`.
    project TEXT NOT NULL,
    agent TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    ended_at INTEGER NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    cost REAL NOT NULL,
    currency TEXT,
    adds INTEGER NOT NULL,
    dels INTEGER NOT NULL,
    files INTEGER NOT NULL,
    allowed INTEGER NOT NULL,
    denied INTEGER NOT NULL,
    stop TEXT NOT NULL
);
CREATE INDEX turns_by_time ON turns(started_at);
CREATE INDEX turns_by_thread ON turns(thread);

CREATE TABLE turn_models (
    turn INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    model TEXT NOT NULL,
    input INTEGER NOT NULL,
    output INTEGER NOT NULL,
    cache_read INTEGER NOT NULL,
    cache_write INTEGER NOT NULL,
    PRIMARY KEY (turn, model)
) WITHOUT ROWID;

CREATE TABLE turn_tools (
    turn INTEGER NOT NULL REFERENCES turns(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    calls INTEGER NOT NULL,
    denied INTEGER NOT NULL,
    PRIMARY KEY (turn, kind)
) WITHOUT ROWID;

-- Each reading of a usage limit that differs from the one before it.
CREATE TABLE limits (
    at INTEGER NOT NULL,
    agent TEXT NOT NULL,
    window TEXT NOT NULL,
    status TEXT NOT NULL,
    used REAL,
    resets_at INTEGER
);
CREATE INDEX limits_by_time ON limits(at);
