-- A host can connect through an outbound proxy (SOCKS5 or HTTP CONNECT).
-- `proxy` holds the non-secret settings as JSON ({"kind","host","port",
-- "username"}); it is NULL for a direct connection and is synced like the
-- other host fields. The proxy password is a host secret: it lives next to
-- the other saved secrets in `host_secrets`, sealed by the same cipher, and
-- is local to the device.
ALTER TABLE hosts ADD COLUMN proxy TEXT;
ALTER TABLE host_secrets ADD COLUMN proxy_password_enc BLOB;
