-- Schema 2: each project's board, an OpenUI Lang program the dashboard draws beside the plan.
-- board_rev starts at 0 (never set) and counts every set and clear.
ALTER TABLE projects ADD COLUMN board TEXT;
ALTER TABLE projects ADD COLUMN board_rev INTEGER NOT NULL DEFAULT 0 CHECK (board_rev >= 0);
