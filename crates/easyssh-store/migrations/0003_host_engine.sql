ALTER TABLE hosts ADD COLUMN engine TEXT CHECK (engine IS NULL OR engine IN ('russh', 'openssh'));
