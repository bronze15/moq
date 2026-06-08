---
title: moq-stats
description: Live stats dashboard for moq-relay
---

# moq-stats

`moq-stats` is a small sidecar that turns a relay's stats into a live web
dashboard: how many sessions are connected, how many streams are live, and how
many viewers each stream has. It subscribes to the relay's `.stats` broadcast
over MoQ, aggregates the latest snapshot, and serves an HTML page plus a JSON
API. It is read-only and stateless, so you can run one (or several) anywhere that
can reach the relay.

## Enable stats on the relay

The dashboard reads data the relay only publishes when stats are enabled. In
`relay.toml`:

```toml
[stats]
enabled = true
# Optional: disambiguate multiple relays sharing a cluster origin.
# node = "sjc/1"
```

Or via the environment / flags: `MOQ_STATS_ENABLED=true` (`--stats-enabled`).
The relay then publishes a `.stats/node` broadcast (the prefix defaults to
`.stats`). See [Configuration](/bin/relay/config).

## Run the dashboard

```bash
moq-stats \
  --url https://relay.example.com \
  --listen 0.0.0.0:8090
```

Then open `http://<host>:8090/`. The page polls `/api/stats` every couple of
seconds and shows:

- **Conectados** - total connected sessions across all auth roots.
- **En vivo** - number of broadcasts currently being transmitted.
- A table of **viewers per stream**, with a dot marking live streams.

Flags:

- `--url <URL>` - the relay to connect to. If the relay requires auth, include a
  token that is allowed to consume the `.stats` prefix, e.g.
  `https://relay.example.com?jwt=<token>` (see [Authentication](/bin/relay/auth)).
- `--listen <ADDR>` - where to serve the dashboard. Defaults to `0.0.0.0:8090`.
- `--stats-broadcast <PATH>` - the relay's stats broadcast path. Defaults to
  `.stats/node`; match it to the relay's `stats.prefix` and `stats.node`.

## JSON API

`GET /api/stats` returns the same data the page renders, for embedding in your
own UI:

```json
{
  "connected": 1342,
  "live_streams": 17,
  "streams": [
    { "id": "sala/ana", "viewers": 230, "live": true },
    { "id": "sala/leo", "viewers": 88, "live": true }
  ]
}
```

## Security

The dashboard has no authentication of its own. Bind it to a private interface
or put it behind your platform's reverse proxy / auth, and don't expose
`--listen` directly to the public internet.
