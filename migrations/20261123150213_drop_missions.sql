-- drop_missions: Missions were taken out for a rethink. Their migration stays,
-- so databases that ran it still open; this takes the table away again.

DROP TABLE IF EXISTS missions;
