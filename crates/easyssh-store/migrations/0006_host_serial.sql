-- A host can be a serial port. `serial` holds the settings as JSON
-- (port path, baud, data bits, parity, stop bits, flow control); it is NULL
-- for SSH hosts. `target_kind` stays 'endpoint' with the port path in
-- `hostname`, which keeps the CHECK constraint of 0001 and means older
-- builds still list the host (they cannot open it).
ALTER TABLE hosts ADD COLUMN serial TEXT;
