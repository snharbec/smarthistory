# The `smarthistory serve` web dashboard

The `smarthistory serve` command starts an HTTP server exposing the same
time-tracking data `project report` prints as text — a per-day, per-project
breakdown of directories, commands, notes, files, and websites — as a JSON API
plus an embedded single-page web UI: an overview of every project active on a
day, a drill-down detail view per project with collapsible files/notes/links
lists, date navigation, and a project search view showing the last 7 days of
tracked time.

## Security: no authentication

**There is no login, no token, no access control of any kind.** Anyone who can
reach the port can read every tracked project's directories, commands, notes
created, and URLs visited, for any day in the database.

Because of this, `smarthistory serve` binds to `127.0.0.1` (loopback only) by
default. Widening the bind — `--host 0.0.0.0`, `--host <your LAN IP>`, or
`serve.host=0.0.0.0` in the config — is an explicit, unauthenticated opt-in; the
server prints a warning to stderr on startup whenever it isn't bound to
loopback. Only do this on a network you trust, and understand that it's
equivalent to publishing your shell/editor/browser activity to everyone on that
network.

## Usage

```sh
smarthistory serve [--port N] [--host ADDRESS]
```

- Runs in the **foreground**. Manage it with a launchd/systemd unit, a terminal
  multiplexer pane, or `smarthistory serve &`, same convention `daemon` uses.
- `--port`/`--host` override `serve.port`/`serve.host` for this invocation.
- Prints the address it's listening on to stderr, so you can confirm it picked
  up the right host/port.

Every request opens its own short-lived, **read-only** database connection — the
dashboard never writes anything, so there's no lock contention with your shell's
normal `smarthistory add` writes, no matter how long the server runs.

## Configuration

| Key          | Default     | Meaning                                            |
| ------------ | ----------- | -------------------------------------------------- |
| `serve.host` | `127.0.0.1` | Default bind address. See the security note above. |
| `serve.port` | `4590`      | Default port.                                      |

```ini
serve.port=8080
```

## API

All endpoints return JSON. All are read-only `GET`s.

### `GET /api/report`

One day's report — the same data `project report` prints, structured.

| Query param    | Default  | Meaning                                                                                                                                                                    |
| -------------- | -------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `day`          | `today`  | `YYYY-MM-DD`, `today`, or `yesterday` — same values `--day` accepts.                                                                                                       |
| `project`      | _(none)_ | Restrict to one project slug. Omit for every project active that day, plus a trailing `"untracked"` entry.                                                                 |
| `min_duration` | `0`      | Only include commands whose derived active duration is at least this many seconds. Doesn't affect the total or directories breakdown — same semantics as `--min-duration`. |

Response shape (one entry in `projects` per project, plus `"untracked"` when
`project` wasn't given and untracked activity exists):

```json
{
  "date": "2026-08-16",
  "projects": [
    {
      "slug": "acme",
      "is_untracked": false,
      "active_secs": 5820,
      "directories": [
        { "directory": "/home/me/work/acme", "active_secs": 5820 }
      ],
      "commands": [
        {
          "time_label": "09:12:04",
          "timestamp": 1786860724,
          "count": 1,
          "active_secs": 340,
          "directory": "/home/me/work/acme",
          "command": "cargo build --release"
        }
      ],
      "notes": ["Standup"],
      "files": {
        "viewed": [],
        "modified": [{ "path": "/home/me/work/acme/src/main.rs", "count": 3 }],
        "created": [],
        "deleted": []
      },
      "websites": [
        {
          "cluster": "github.com",
          "links": [
            { "title": "acme/acme", "url": "https://github.com/acme/acme" }
          ]
        }
      ]
    }
  ]
}
```

A `CommandGroupJson` entry's `timestamp` is only present when `count == 1` — a
collapsed group (the same command run more than once that day, same `"Nx"`
convention the CLI table uses) has no single timestamp left to show.

### `GET /api/history`

The trailing N days' active-time totals for one project — the data source for
the dashboard's search view.

| Query param | Default      | Meaning                                                     |
| ----------- | ------------ | ----------------------------------------------------------- |
| `project`   | _(required)_ | Project slug.                                               |
| `days`      | `7`          | How many trailing days, including today (clamped to 1–366). |

```json
[
  { "date": "2026-08-10", "active_secs": 0 },
  { "date": "2026-08-11", "active_secs": 3600 },
  { "date": "2026-08-16", "active_secs": 5820 }
]
```

### `GET /api/projects`

Every known project slug — every configured `project.<slug>.dir` entry, unioned
with every distinct slug that has ever appeared in the time-tracking database
(so a project whose config entry was since removed or renamed is still
browsable). The dashboard's search box's data source.

```json
["acme", "other"]
```

## The web UI

Anything that isn't `/api/*` falls back to the dashboard's HTML shell — `/`,
`/day/2026-08-16`, `/day/2026-08-16/project/acme`, `/project/acme`, or any of
those after a hard refresh. The page's own JavaScript reads `location.pathname`
to decide what to render, and drives further navigation via
`history.pushState`/`popstate` — so the browser's back/forward buttons navigate
for real, and a direct link to a specific day/project always works.

The entire UI — HTML, CSS, and JS — is a single file compiled directly into the
`smarthistory` binary (no build step, no CDN, no separate assets to ship). It's
plain JavaScript with no framework: file/note/link lists use native
`<details>`/`<summary>` for collapse/expand, and the layout follows
`prefers-color-scheme` for light/dark.

## Performance

Loading a day with browser history or JIRA-linked activity is unavoidably slow
_the first time_ in a while: assembling the websites section reads your
browser's history database (a filesystem copy of the whole file — Chrome/
Firefox/Safari history files are typically tens to hundreds of MB) and, for any
JIRA-linked visit, looks up that issue's labels over the network. Both are
cached process-wide (not per-request) for the life of the `smarthistory serve`
process:

- Browser history/bookmarks: cached 30 seconds, keyed by the resolved source
  list. Clicking through several days in one sitting only pays for the copy once
  every 30 seconds, not once per day.
- JIRA issue labels: cached 5 minutes per issue key, shared across every
  request. An issue referenced from multiple days' visits only costs one REST
  round-trip.

Every request still opens its own short-lived, read-only database connection
(see above) and runs off the async runtime's own worker threads — `axum`
dispatches each handler onto `tokio`'s blocking-thread pool, since the
underlying work (SQLite queries, the browser history copy, and any JIRA REST
call) is synchronous, not `async`-native.
