-- What a host is for (production, staging, testing, development); additive,
-- NULL means unmarked.
ALTER TABLE hosts ADD COLUMN environment TEXT CHECK (environment IS NULL OR environment IN ('production', 'staging', 'testing', 'development'));
