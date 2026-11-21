CREATE TABLE projects (
    id INTEGER PRIMARY KEY,
    -- The folder, or '' for regular chats.
    path TEXT NOT NULL UNIQUE
);

CREATE TABLE threads (
    id INTEGER PRIMARY KEY,
    project INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    -- Where it sits in the project's list as stored; ties in sorting keep it.
    position INTEGER NOT NULL,
    title TEXT NOT NULL,
    -- By name, since the configured list can change between runs.
    agent TEXT NOT NULL,
    state TEXT NOT NULL,
    unread INTEGER NOT NULL,
    session TEXT,
    picks TEXT NOT NULL,
    worktrees TEXT NOT NULL,
    plan TEXT NOT NULL,
    usage TEXT,
    cost TEXT,
    -- When a turn still running at the save began.
    turn_began INTEGER,
    created_at INTEGER,
    active_at INTEGER,
    -- When the row was written: about when the agent last did anything.
    saved_at INTEGER NOT NULL
);
CREATE INDEX threads_by_project ON threads(project, position);

CREATE TABLE blocks (
    thread INTEGER NOT NULL REFERENCES threads(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    body TEXT NOT NULL,
    PRIMARY KEY (thread, seq)
) WITHOUT ROWID;

-- What was said each way, by `text_row`. Trigrams find any run of three or
-- more characters, ignoring case, as typing into the search box expects.
CREATE VIRTUAL TABLE block_text USING fts5(text, tokenize = 'trigram');
