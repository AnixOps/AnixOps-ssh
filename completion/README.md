# Completion specs

AnixOps SSH completes commands in the terminal from small JSON files that
describe a command: its subcommands, options and arguments. The format is
open, the specs are written by hand for this project, and anyone can add or
fix one with a pull request.

```
completion/
  schema/spec.schema.json   the JSON Schema every spec must satisfy
  specs/<command>.json      one file per command
  index.json                command name -> file (generated, see below)
```

## A spec

```json
{
  "$schema": "../schema/spec.schema.json",
  "name": "git",
  "description": "Distributed version control system",
  "options": [
    { "name": "-C", "description": "Run as if started in this directory",
      "args": [{ "name": "path", "template": "folders" }] }
  ],
  "subcommands": [
    {
      "name": "commit",
      "description": "Record changes to the repository",
      "options": [
        { "name": ["-m", "--message"], "description": "Use this commit message",
          "args": [{ "name": "msg" }], "repeatable": true },
        { "name": "--amend", "description": "Replace the tip commit" }
      ],
      "args": [{ "name": "path", "template": "filepaths", "optional": true, "variadic": true }]
    }
  ]
}
```

### Commands and subcommands

| Field | Meaning |
| --- | --- |
| `name` | The word typed. A list gives aliases; the first is the primary name (and the file name for the top level). |
| `description` | One short sentence, at most 100 characters, English. Shown next to the suggestion. |
| `options` | Options of this command (see below). |
| `subcommands` | The same object again, recursively. Subcommand words are read before positional arguments. |
| `args` | Positional arguments, in order. |
| `parser` | How words are read (see below). Inherited by subcommands, which can override it. |

### Options

| Field | Meaning |
| --- | --- |
| `name` | `"--verbose"`, or a list such as `["-v", "--verbose"]`. Names start with `-`. |
| `description` | Required. |
| `args` | The values the option takes, in order. Leave out for a flag. An argument with `"optional": true` makes the value optional. |
| `repeatable` | May be given more than once. Other options are offered only once. |
| `requiresEquals` | The value must be attached as `--name=value`. The completion inserts the `=`. |
| `exclusiveWith` | Option names that cannot be combined with this one. They are hidden once one of them is used. Names must exist in the same command. |
| `persistent` | Also valid in every subcommand (like `kubectl -n`). |

### Arguments

| Field | Meaning |
| --- | --- |
| `name`, `description` | Shown when the list is empty and in help. |
| `optional` | May be left out. |
| `variadic` | Takes any number of values; must be the last argument. |
| `template` | Where values come from: one name or a list (see below). |
| `suggestions` | Fixed values: strings, or `{ "name", "description" }`. |

Templates:

| Template | Values |
| --- | --- |
| `filepaths` | Files and folders on the machine the shell runs on. In a remote session they come from the host's SFTP channel. |
| `folders` | The same, folders only. |
| `remote-paths` | Paths on the session's host; the same as `filepaths` in a remote session. |
| `history` | Words the user typed earlier after the same command, on the same host. |
| `hosts` | Names of the hosts saved in the app. |
| `command` | A command name. Everything after it is read as that command's own line, so `sudo systemctl restart` completes `systemctl`. |

### Parser directives

| Field | Default | Meaning |
| --- | --- | --- |
| `flagClustering` | `true` | POSIX clustering: `-xzvf` is `-x -z -v -f`, and `-n5` is `-n 5`. Set `false` for programs with long single dash options, like `find -name`. |
| `optionsAfterDoubleDash` | `false` | By default `--` ends the options. |
| `optionsAfterArgs` | `true` | Set `false` when everything after the first positional argument belongs to something else (`sudo`, `docker run IMAGE ...`). |

## Contributing a spec

1. Add `completion/specs/<name>.json`, where `<name>` is the first (primary)
   name in the spec. Start from a similar spec. Put `"$schema": "../schema/spec.schema.json"` at the top for editor help.
2. Write everything yourself from the program's documentation or `--help`
   output. **Do not copy spec files from other projects.** Only list options
   and subcommands you are sure exist; leave out the rest.
3. Keep descriptions short, in English, and without a trailing period.
4. List the most used options and subcommands first: ties are shown in file
   order.
5. Run `npm run check:specs -- --write`. It validates every spec against the
   schema, rejects duplicate names (including aliases, options and
   subcommands) and rewrites `index.json`. CI runs `npm run check:specs`
   without `--write` and fails when the index is stale.
6. Run `npm test` and open a pull request.

Specs are loaded lazily, one per command, so a larger set does not slow the
app down.

## Privacy

History suggestions come from what the user ran in this app, kept per host
in memory and local storage only. They are never synced, lines that look like
they contain a secret are skipped, and "Remember command history" in the
settings turns them off and clears them.
