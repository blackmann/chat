-- backfill_turns: turns rebuilt from threads saved before turns were counted.
-- The rows themselves are written by the migration's `after` hook in
-- `store.rs`, which reads each thread's saved blocks: their timing, tools,
-- edits and permission answers. Tokens weren't kept then, so rebuilt turns
-- have none, and their cost is a share of the thread's.
ALTER TABLE turns ADD COLUMN rebuilt INTEGER NOT NULL DEFAULT 0;
