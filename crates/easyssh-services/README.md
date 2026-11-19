# easyssh-services

Imports hosts from services the user already runs. Each importer fetches over
HTTPS with the user's API credential, maps the answer to
`easyssh_store::import::ImportHost` and stops there: the preview ("already
stored" marks) and the merge (`import_apply`, duplicate policy, group modes)
are the store's, the same code a file import uses.

```rust
let remembered = easyssh_services::settings::remembered(&store, Service::Control);
let secret = easyssh_services::credential(&store, &vault, Service::Control, typed)?;
let preview = easyssh_services::fetch(Service::Control, &form, &secret)?.preview(&store)?;
```

## Rules

- HTTPS only; `http://` is accepted for `localhost`, `127.0.0.1` and `::1`
  (testing). Credentials, a query or a fragment in the address are refused.
  Redirects are not followed (a redirect must not carry a credential
  elsewhere). Replies are capped at 8 MiB, requests time out after 10 s to
  connect and 30 s to read.
- The credential is sealed by the vault in the settings slot
  `service.<id>.secret` (AAD `easyssh/v1/service-secret/<id>`), never part of
  the remembered form, never in a URL, a log line or an error message (it is
  also scrubbed from error text the service echoes back).
- Errors are for people: `ServiceError` has a stable `code()` and a message.
- Tests run against an in-process fake server on 127.0.0.1; nothing leaves the
  machine.

## Source keys and re-sync

Every imported host carries `source`, a stable key of the form
`<service>:<origin>#<id>` (AnixOps Control: `control:https://panel.example.com#12`).
`import_apply` records `source -> host id` in the setting `import.sources`.
Importing again finds the host by that key first (so a changed address updates
the host instead of adding a second one) and falls back to hostname, port and
user. A matched host gets its name, address, port, group, tags and notes
refreshed; its id, user, favorite, colour and everything the user set stay.
Hosts the service no longer lists are returned in the preview as `removed`;
nothing is ever deleted.

## AnixOps Control

`GET {panel}/api/v4/admin/nodes?page=N&page_size=100` with
`Authorization: Bearer anixadm_...` (an admin API token, scope `read`
suffices; the panel refuses a token in a URL and audits it). Reply
`{"data":{"total":n,"list":[node...]}}`; errors
`{"error":{"code","message"}}` (401 `api_token_invalid`, 403
`api_token_forbidden`, 429 `api_token_rate_limited` with `Retry-After`).

| node field | host |
|---|---|
| `name` | name (`node-<id>` when empty) |
| `host`, else `server_ip` | address; a node with neither is skipped with a warning |
| port | 22 |
| default user (dialog, `root`) | user |
| `node_group_name`, else `AnixOps Control` | group, under a top group named after the panel host |
| `tags` (JSON array in a string; arrays and comma lists also read) | tags |
| `approval` not `approved` (empty counts as approved) | skipped, counted in one warning |

Pages are read until the panel's `total` is reached, the list runs out, or 50
pages (5,000 nodes) have been read.

## JumpServer

`GET {url}/api/v1/assets/hosts/?limit=100&offset=N`, the request AnixOps'
fleet tool (`scripts/jms.py` in anixops-ansible) makes. Every request is
signed with an access key: HTTP Signature (draft-cavage),
`Authorization: Signature keyId="<key id>",algorithm="hmac-sha256",headers="(request-target) accept date",signature="<base64>"`,
where the signature is HMAC-SHA256 (key = the access key secret) over

```
(request-target): get /api/v1/assets/hosts/?limit=100&offset=0
accept: application/json
date: Thu, 01 Jan 2026 00:00:00 GMT
```

plus the `Date` header and `X-JMS-ORG` (default organization
`00000000-0000-0000-0000-000000000002`). A test pins a vector computed with
python3 reproducing `SignatureAuth`. The reply is `{"count","next","results"}`
or a bare list.

| asset field | host |
|---|---|
| `name`, else `address` | name |
| `is_active: false` | skipped (disabled in JumpServer) |
| `protocols` with `ssh` (port from it) | SSH port; protocols without SSH (RDP, VNC, databases): skipped with a warning; no `protocols` at all: 22, unless the platform is Windows |
| `nodes_display[0]` without its root node (`/Default/Prod/Web` is `Prod/Web`) | group, under a top group named after the JumpServer host |
| asset `id` | source key `jumpserver:<origin>#<org>#<id>` |

**Gateway (default on).** Every host connects to the KoKo gateway (host
defaults to the JumpServer's host, port 2222) with KoKo's direct asset login
in the SSH user name: `<jms user>@<account>@<asset address>`, the same as
`ssh -p2222 admin@root@192.168.1.10@jms.example.com`. The account is the
form's default user (asset account, `root`); the address is used because it is
unambiguous (KoKo also accepts the asset name). This is the form JumpServer
documents for 2.x and 3.x; very old releases may differ. With the account
empty the user is only `<jms user>` and the asset is picked in KoKo's menu
(`/` and part of the name, Enter). The asset's name and address are in the
host's notes. The JumpServer password and MFA code are not
stored: KoKo asks for them as keyboard-interactive prompts, which the app's
prompt dialog shows. All hosts of one gateway share host, port and user, so
they are told apart by their source key (menu mode shares host, port and user) (`import_apply` never matches a
service host by address against a host another source created). With the
gateway off, each host connects to the asset's address with the default user.

The login form lives in `jumpserver::gateway_login`. An `@` in the JumpServer
user name or the account is refused (the login would be ambiguous).
