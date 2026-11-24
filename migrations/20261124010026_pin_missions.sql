-- pin_missions: missions are back, redesigned: tiny games offered from the
-- status bar's mail icon. One row per mission, kept after it's done for the
-- streak. `phase` is offered, waiting, recall or done; `turns_left` counts
-- down while waiting, `tries` the wrong answers while recalling, and `solved`
-- says how a done one went.

CREATE TABLE missions (
    id INTEGER PRIMARY KEY,
    kind TEXT NOT NULL,
    pin TEXT NOT NULL,
    phase TEXT NOT NULL,
    wait_turns INTEGER NOT NULL,
    turns_left INTEGER NOT NULL,
    tries INTEGER NOT NULL,
    guess TEXT,
    solved INTEGER,
    created_at INTEGER NOT NULL,
    ended_at INTEGER
);
