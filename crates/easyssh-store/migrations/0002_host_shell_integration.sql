ALTER TABLE hosts ADD COLUMN shell_integration INTEGER NOT NULL DEFAULT 0 CHECK (shell_integration IN (0, 1));
