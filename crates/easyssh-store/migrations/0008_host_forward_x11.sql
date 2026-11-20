-- X11 forwarding (`ssh -X`) as a per-host toggle; additive, off by default.
ALTER TABLE hosts ADD COLUMN forward_x11 INTEGER NOT NULL DEFAULT 0 CHECK (forward_x11 IN (0, 1));
