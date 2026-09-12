# Changelog

## [0.5.0] - 2026-09-13

### Changed — Qlik Cloud is verified against a live tenant
The adapter refused five of its seven methods on a single claim: that "Qlik apps are read
through the Engine JSON API over a WebSocket, not REST". Live testing split that claim in
half, and the wrong half was costing real capability.

**An app's data model is plain REST.** `GET /api/v1/apps/{id}/data/metadata` returns the
tables with row counts and every field with its type tags. `bi_list_datasets` and
`bi_describe_dataset` now work — measured on a trial tenant as three business tables of
632,313, 245 and 30 rows, and a 14-field schema.

**Sheets and visuals genuinely are not.** `/apps/{id}/objects` and `/apps/{id}/sheets` both
answer 404. That limit is now measured rather than assumed, and the verification asserts the
refusals of `bi_chart_data`, `bi_drill_down` and `bi_insights` alongside the successes.

**An app described itself as a GUID**, the same defect the Power BI adapter had.
`attributes.name` is one REST call away.

**Every field is now reported as groupable**, contrary to the other adapters and
deliberately so. Qlik's associative model makes any field selectable as a dimension, and the
metadata carries nothing to infer a measure from: `Date_year` has 2 distinct values and
`Quantity` has 3, so cardinality cannot separate them, and there is no `SummarizeBy`
equivalent. Marking `Date_year` ungroupable would stop an agent grouping by year on a sales
model. `kind` still carries the numeric signal.

A dataset id is `{appId}:{tableName}`, because Qlik has no single object for a queryable
table: an app holds tables, while `/items?resourceType=dataset` lists `.qvd` and `.txt`
files with no schema endpoint of their own. Qlik's own bookkeeping is excluded — tables
flagged `is_system` and fields tagged `$system` describe the model, not the business.

### Added — `scripts/verify-qlik.py`
29 checks against a live tenant over real MCP stdio, including that a wrong table name is
refused by listing the tables that do exist rather than returning an empty column list.

## [0.4.0] - 2026-09-13

### Changed — Power BI is verified against a live tenant
The first commercial adapter proven rather than reviewed, and it needed five corrections.
Every one looked reasonable against the documentation.

**Both artefacts are listed.** The adapter returned only reports, on the reasoning that
"reports are what people mean by a dashboard". Against a real tenant that hid the thing
its owner called their dashboard: the report's pages were named "Page 1" to "Page 5",
while the dashboard beside it held 14 tiles named "No. of Houses with Water",
"Satisfied with Water Services", "Responses". `bi_get_dashboard` now returns tiles or
pages depending on which artefact the id names.

**`INFO.COLUMNS()` does not work.** It answers HTTP 400 with "Failed to execute the DAX
query." and error code 3239575574. `INFO.VIEW.COLUMNS()` works, and also reports
`IsHidden` — a real model carried 9 hidden columns out of 28, including two RowNumber
internals and a hidden date table, none of which an agent should be offered as something
to group by.

**A dataset described itself as a GUID.** `name` was filled with the id, so
`bi_list_datasets` said "Water Survey" and `bi_describe_dataset` said
"a bare GUID" for the same model.

**DAX result columns were read by position.** They arrive as a JSON object, and with
serde_json's default map they came back alphabetically: a query asking for name, kind and
hidden returned `["[hidden]", "[kind]", "[name]"]`. Reading row[0] as the name read a
boolean. Fixed at both ends — aliases are now looked up by name, and the `preserve_order`
feature keeps the query's order for anyone reading `table.columns`.

**`webUrl` means two different things.** `GET /dashboards` reports a page;
`GET /dashboards/{id}` reports a chrome-less embed surface, under the same field name.
`bi_dashboard_url` now returns something a person can open.

Also: a failure to read a model's shape used to be swallowed into an empty column list,
which reads as "this model has no columns" and is what hid the broken metadata query. It
now says what it tried and what happened. And a tenant-level export block —
`403 "Export report to image is disabled on tenant level"` — is explained as an
administrator setting with an alternative, rather than passed through as a raw status that
invites a retry.

### Added — `scripts/verify-powerbi.py` and `scripts/powerbi-token.py`
32 checks against a live tenant over real MCP stdio. The token helper signs in by device
code against a Microsoft first-party public client already pre-authorised for the Power BI
API, so verifying needs nothing registered in a tenant and grants nothing the user cannot
already do.

The script asserts refusals as well as successes. Power BI's REST API exposes no endpoint
returning the data behind a tile or a page, so `bi_chart_data`, `bi_insights`,
`bi_drill_down` and `bi_render_chart` must decline; a backend that improvised numbers
there would be worse than one that declines. `bi_query` with DAX is the route that works.

### Changed
`serde_json` now uses `preserve_order`, so a JSON object's keys keep their original order
throughout. All tests pass with it, and it removes a class of bug where reading a result
positionally works for one query and silently reads the wrong field in another.

## [0.3.0] - 2026-09-13

### Added — Metabase renews its own session
Metabase previously took only `METABASE_TOKEN`, so a run that outlived its session
failed partway through with `401 Unauthenticated` and nothing to do about it. It now
also takes `METABASE_USERNAME` and `METABASE_PASSWORD`, and obtains a session itself.

The mechanism differs from Superset's deliberately. A Superset access token is a JWT,
so its expiry can be read from the token and a refresh timed to beat it. A Metabase
session id is an opaque UUID carrying no expiry, so a lapse cannot be anticipated —
the only reliable signal is the platform refusing it. Recovery is therefore reactive:
a rejected request triggers one re-authentication and one retry. Retried exactly once,
because a second failure means the credentials are wrong and retrying would turn a
clear rejection into a loop.

Measured against a live instance rather than read from the documentation:
`POST /api/session` answers with exactly `{"id": "<uuid>"}`, and a rejected session
answers `401` with the bare body `Unauthenticated` — not JSON, so nothing useful can
be parsed from it.

### Added — a failed call carries its status as data
`http::status_of` and `http::is_unauthorized` read the HTTP status from an error
directly. Recovery that depends on matching an error message breaks silently when the
message is reworded, and Metabase's one-word body gives a parser nothing to work with.

The status is attached as the error's cause rather than its context, so the readable
message stays the headline — an earlier arrangement made every failure display as
"HTTP status 401" and buried the platform's own explanation of which permission was
missing.

### Added — `scripts/verify-metabase.py`
28 checks against a live Metabase over real MCP stdio, matching the Superset script.
Includes asserting the *refusal* of `bi_export_dashboard_image`: Metabase renders
dashboards client-side, so the honest answer is a structured error naming the limit
rather than a blank or invented image.

### Changed
The README no longer claims Metabase is unverified against a live instance. It is, and
the two corrections live testing produced are now recorded beside Superset's three.

## [0.2.0] - 2026-09-12

### Added — memory, so a correction is learned once
A data platform is full of facts its schema does not contain: which of four
similarly-named charts people mean by "revenue", that a filter wants an exact
value rather than a fuzzy match, that a dataset id is a table id and not a
database id. An agent rediscovers each of those the expensive way every session
unless something writes them down. OpenAI published the measurement that makes
the case — the same question against their internal data agent took **22m 41s
without memory and 1m 22s with it**.

- **`bi_remember`** records a correction, replacing any earlier note on the same
  subject so a corrected correction does not sit beside its replacement.
- **`bi_recall`** offers back the notes bearing on a question, ranked by word
  overlap and by how often each has proved useful before.
- **`bi_forget`** removes one, because a wrong correction is worse than none.

Notes are **corrections, not knowledge**. A figure would be wrong next month, and
every number should come from a fresh query.

Scoped per backend: a Superset chart id means nothing to Metabase, so it is never
offered there. Persisted through a temp-file rename, so an interrupted write
cannot destroy a readable file. Bounded at 500 notes and 600 characters each.
Retrieval is keyword overlap rather than embeddings, which keeps it deterministic,
testable offline and free of an API key. An unwritable location degrades to
session-only with a warning rather than failing a read-only server.

Notes are written by a model and later read by one, so they are returned labelled
as recorded observations to verify, never as instructions.

15 tools. 46 tests (12 unit + 32 integration + 4 manifest), including one that
proves a note survives a process restart.

## [0.1.2] - 2026-09-12

### Fixed — Metabase queries ran against the wrong id space
`bi_query` passed the dataset id straight through as the `database` field of
`/api/dataset`. Metabase's datasets are **tables**, whose ids are unrelated to
database ids, so a valid dataset id produced
`HTTP 500 Assert failed: (keyword? driver)` — Metabase failing to resolve a driver
for a database that does not exist. The message named neither the cause nor the
remedy, and a live agent hit it on a dataset id it had read from
`bi_list_datasets` moments earlier.

The table's own `db_id` is now resolved before the query runs, and an id that is
not a table fails with a message saying so instead of silently falling back to
database 1. 38 tests (12 unit + 22 integration + 4 manifest).

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
