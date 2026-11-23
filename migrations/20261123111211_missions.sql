-- missions: challenges taken on while agents work. They belong to the app,
-- not to a thread or project. `phase` is the mission's phase as JSON; the
-- puzzle itself is made again from `kind`, `seed` and `level`. `score` and
-- `ended_at` are filled in once it's done, for totals and streaks.

CREATE TABLE missions (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    seed INTEGER NOT NULL,
    level INTEGER NOT NULL,
    started_at INTEGER NOT NULL,
    phase TEXT NOT NULL,
    answer TEXT,
    score INTEGER,
    ended_at INTEGER
);
CREATE INDEX missions_by_end ON missions(ended_at);
