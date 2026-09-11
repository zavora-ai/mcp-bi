# Changelog

## [0.1.1] - 2026-09-11

### Fixed — a session no longer dies when the token does
A Superset access token is valid for **15 minutes**, which is shorter than a real
analysis session. A live agent run failed partway through with
`HTTP 401 {"msg":"Token has expired"}`, having already produced a plan and read a
dashboard; it then went looking at the desktop to work out why, which is exactly
what an analyst should not do.

- `SUPERSET_USERNAME` and `SUPERSET_PASSWORD` (with optional `SUPERSET_AUTH_PROVIDER`,
  default `db`) let the server obtain its own token and replace it 60 seconds before
  the `exp` claim in the token it is holding. Every request goes through one `auth()`
  seam, so no call site can be forgotten.
- A supplied `SUPERSET_TOKEN` is still honoured and still used until it expires. It
  may now be combined with credentials, which is the durable configuration: no login
  on startup, no failure later.
- An expired token with no credentials configured now fails **before** the request
  with an error stating the 15-minute lifetime and naming the two variables that fix
  it, rather than relaying Superset's `{"msg":"Token has expired"}`.
- A token that is not a JWT, or carries no `exp`, is trusted as given — only the
  server can judge an opaque token.

Verified against live Superset with **no `SUPERSET_TOKEN` set at all**: the server
logged in itself and all 27 checks in `scripts/verify-superset.py` passed. 36 tests
(10 unit + 22 integration + 4 manifest), up from 31.

## [0.1.0] - 2026-09-11

Initial release. 12 tools over 8 backends, open source first.

### Added
- **Apache Superset** as the primary backend (ASF-licensed), with **Metabase** alongside it. Both are read through their REST APIs with a bearer token.
- **Power BI, Tableau, Looker, Qlik Sense and Amazon QuickSight** as optional backends behind the same `BiBackend` trait, selected by `BI_BACKEND`.
- **Deterministic memory backend** as the default, so the server runs and is testable with no credentials: 3 dashboards, 6 tickers and 30 days of seeded data from a SplitMix64 generator.
- **`bi_insights`** — direction, absolute and relative change, extremes, mean, standard deviation and outliers computed over a chart's own rows.
- **`bi_render_chart`** — renders bar, line and pie charts to PNG server-side with `charts-rs`, no browser and no headless Chrome, and returns the statistics alongside the image.
- **`bi_drill_down`** — applies filters and a breakdown dimension to an existing chart, so an agent can narrow a visual the way a person clicks into it.
- **`bi_dashboard_url`** — builds a deep link that opens a dashboard with filters already applied, for handing a finding back to a person.
- Injectable `Http` trait with a `Recorded` fixture client behind the `recorded-http` feature, so vendor adapters are tested without a network.
- `scripts/verify-superset.py` exercises every tool against a live Superset over MCP stdio: 27 checks.

### Design
- **Numbers are authoritative, pixels are for people.** Every tool that returns an image also returns the rows or statistics it was drawn from, so a claim is never sourced from a rendering.
- **A missing capability is an error that names the platform and the workaround, never an empty result.** "No rows" is a worse claim than "not supported on this platform".
- **No write surface.** `writes_allowed = "none"`; a dashboard is changed through the platform's own review, not by an agent.

### Verified against live Apache Superset
Live testing corrected three API shapes that recorded fixtures had happily confirmed:
- `GET /api/v1/chart/{id}/data/` returns `400 Chart has no query context saved` for UI-built charts, which have no saved query context. The query has to be rebuilt from the chart's `params` and posted to `/api/v1/chart/data`.
- `POST /api/v1/chart/{id}/data/` returns `405 Method Not Allowed` — the POST route carries no chart id.
- SQL Lab rejects `limit` outright (the field is `queryLimit`) and is CSRF-protected, needing `X-CSRFToken` **and** the session cookie issued with it. The chart data endpoint needs neither.

All three are fixed, and the fixtures were rewritten to encode the verified contract rather than the assumed one.

### Notes
- 31 tests (10 unit + 17 integration + 4 manifest); clippy clean; 27 live checks pass against Superset's own example dashboards.
- The five commercial adapters are implemented and unit-tested against recorded fixtures but **not yet proven against live tenants**. Superset needed three corrections once real, so expect at least one surprise per platform.
