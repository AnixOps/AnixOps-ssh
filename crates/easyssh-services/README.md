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
