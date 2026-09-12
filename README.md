# Business Intelligence MCP Server

[![Crates.io](https://img.shields.io/crates/v/mcp-bi.svg)](https://crates.io/crates/mcp-bi)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![ADK-Rust Enterprise](https://img.shields.io/badge/ADK--Rust-Enterprise-purple.svg)](https://enterprise.adk-rust.com)
[![Registry Ready](https://img.shields.io/badge/ADK_Registry-Ready-green.svg)](https://www.zavora.ai)

Read, drill into and draw the dashboards a business already has. Open source first:
**Apache Superset** is the primary backend, with Metabase alongside it, and the five
most widely deployed commercial platforms as optional backends behind the same
interface.

<p align="center">
  <img src="https://raw.githubusercontent.com/zavora-ai/mcp-bi/main/docs/architecture.svg" alt="Business Intelligence MCP Architecture" width="780"/>
</p>

An agent writes one set of calls. Swapping vendor does not change them.

## Backends

Selected with `BI_BACKEND`. The default needs no credentials and no network.

| `BI_BACKEND` | Platform | Open source | Dashboards | Chart data | Query | Drill | Image |
|---|---|---|---|---|---|---|---|
| `memory` *(default)* | Seeded fixture | — | ✅ | ✅ | — | ✅ | — |
| `superset` | **Apache Superset** | ✅ | ✅ | ✅ | SQL | ✅ | ✅ |
| `metabase` | Metabase | ✅ | ✅ | ✅ | SQL | — | — |
| `powerbi` | Microsoft Power BI | — | ✅ | — | DAX | — | async |
| `tableau` | Salesforce Tableau | — | ✅ | ✅ | — | — | ✅ |
| `looker` | Google Looker | — | ✅ | ✅ | explore | — | — |
| `qlik` | Qlik Sense | — | ✅ | — | — | — | — |
| `quicksight` | Amazon QuickSight | — | ✅ | — | — | — | — |

The gaps are real and reported. `bi_backend_info` returns them, and a call into one
fails with a message naming the platform and the way round it — never an empty
result, because "no rows" is a different and much worse claim than "not supported".

## Two rules the server enforces

**Numbers are authoritative; pixels are for people.** Every visual has a data route.
A model handed only a dashboard image will describe a trend it did not measure, so
`bi_render_chart` returns the picture *and* the statistics behind it, and
`bi_insights` returns those statistics alone. Narration cites a figure.

**A fixture is labelled as one.** The default backend is seeded and says so. It is
identical on every run, which is what lets a conclusion reached today be re-derived
tomorrow and compared.

## Tools (12)

| Tool | Purpose |
|---|---|
| `bi_backend_info` | Which platform is connected and what it can do. Call first |
| `bi_list_dashboards` | The saved dashboards |
| `bi_get_dashboard` | One dashboard: charts, drillable dimensions, URL |
| `bi_list_datasets` | Datasets and semantic models |
| `bi_describe_dataset` | Columns, types, which are groupable |
| `bi_chart_data` | The rows behind one chart |
| `bi_drill_down` | Narrow by filters, break down by a dimension, with rows before/after |
| `bi_query` | SQL, DAX or an explore, depending on platform |
| `bi_insights` | Direction, change, min/max with labels, mean, deviation, outliers, gaps |
| `bi_render_chart` | Draw a chart to PNG with `charts-rs`, returned with its statistics |
| `bi_export_dashboard_image` | Platform-rendered dashboard image, where offered |
| `bi_dashboard_url` | Deep link with filters applied, for a browser or a screenshot |

Nothing writes. A dashboard is changed through the platform's own review, not by an
agent, so `writes_allowed = "none"` and there is no approval surface.

## Run it

```sh
cargo run                       # seeded fixture: three dashboards, no setup
BI_BACKEND=superset \
  SUPERSET_URL=http://localhost:8088 \
  SUPERSET_USERNAME=admin SUPERSET_PASSWORD=... cargo run
```

**Give Superset a username and password, not a token.** A Superset access token is
valid for **15 minutes** — measured, not assumed — which is shorter than a real
analysis session. With credentials the server mints its own token and replaces it a
minute before it expires, so a long session cannot die partway through. A supplied
`SUPERSET_TOKEN` still works and is used until it expires; after that, a request
fails with an error naming this remedy rather than passing along Superset's
`{"msg":"Token has expired"}`.

| Backend | Variables |
|---|---|
| `superset` | `SUPERSET_URL`, and either `SUPERSET_USERNAME` + `SUPERSET_PASSWORD` (preferred, self-refreshing) or `SUPERSET_TOKEN`. Optional `SUPERSET_AUTH_PROVIDER` (default `db`) |
| `metabase` | `METABASE_URL`, and either `METABASE_USERNAME` + `METABASE_PASSWORD` (preferred — the server renews its own session) or `METABASE_TOKEN` |
| `powerbi` | `POWERBI_TOKEN` (obtain with `scripts/powerbi-token.py`), optional `POWERBI_GROUP_ID` for a workspace other than My Workspace, `POWERBI_API` |
| `tableau` | `TABLEAU_URL`, `TABLEAU_TOKEN`, `TABLEAU_SITE_ID` |
| `looker` | `LOOKER_URL`, `LOOKER_TOKEN` |
| `qlik` | `QLIK_URL`, `QLIK_TOKEN` (Profile settings ▸ API keys). A dataset id is `{appId}:{tableName}` |
| `quicksight` | `AWS_ACCOUNT_ID`, `QUICKSIGHT_TOKEN`, optional `AWS_REGION` |

## Rendering

`charts-rs` draws line, bar, horizontal bar, pie and table charts to PNG on the
server — no browser, no JavaScript, no headless Chrome. Ten themes; `grafana` is the
default because it reads well on a dark console. A gap in a series stays a gap rather
than dropping to zero, which would invent a crash that never happened.

## Platform notes worth knowing before you plan

- **Power BI's real-time path is closing.** Push and streaming semantic models can no
  longer be created and retire on 2027-10-31. What is stable is reading a model with
  DAX and exporting a report, which is what this adapter uses. For streaming,
  Microsoft points at Fabric Real-Time Intelligence.
- **QuickSight has no API for a visual's data.** Discovery and deep links only.
- **Qlik reads through the Engine API over a WebSocket**, not REST, so only discovery
  and deep links are wired.
- **Superset thumbnails need the `THUMBNAILS` feature flag and a Celery worker.**
  Without them the platform's own error is returned rather than a blank image.

## Testing

```sh
cargo test --features recorded-http
```

27 tests, no credentials and no network. The vendor adapters go through an injectable
HTTP trait, so a test can assert that Superset asks the right endpoint and sends its
token as a header — with no Superset to hand.

## Verified against a live Apache Superset

```sh
docker run -d --name superset-bi -p 8088:8088 \
  -e SUPERSET_SECRET_KEY=local-only apache/superset:latest
docker exec superset-bi superset db upgrade
docker exec superset-bi superset fab create-admin --username admin --firstname A \
  --lastname U --email a@example.invalid --password admin
docker exec superset-bi superset init
docker exec superset-bi superset load_examples          # 9 real dashboards

export SUPERSET_URL=http://localhost:8088
export SUPERSET_USERNAME=admin SUPERSET_PASSWORD=admin

python3 scripts/verify-superset.py ./target/release/mcp-bi
```

All 27 checks pass against Superset's own example content: 9 dashboards, 21 datasets,
1,000 rows of live chart data, real SQL through SQL Lab, and a 31 KB PNG rendered from
those rows. A filter on *Games per Genre* narrows 12 rows to 1.

Three things that live testing corrected, and a reading of the docs would not:

| Assumption | Reality |
|---|---|
| `GET /api/v1/chart/{id}/data/` returns a chart's data | `400 Chart has no query context saved` — charts built in the UI have none. The query must be rebuilt from the chart's `params` and posted to `POST /api/v1/chart/data` |
| `POST /api/v1/chart/{id}/data/` accepts a filtered query | `405 Method Not Allowed`. The POST route has no chart id |
| SQL Lab takes `limit` and a bearer token | `limit` is rejected outright — the field is `queryLimit` — and SQL Lab is CSRF-protected, needing `X-CSRFToken` *and* the session cookie issued with it. The chart data endpoint is not |

The fixture tests now encode these shapes, so a regression would be caught without
Superset running.

## Verified against a live Metabase

```sh
docker run -d --name metabase-bi -p 3000:3000 metabase/metabase:latest
# Complete the setup wizard once, then add the bundled Sample Database.

export METABASE_URL=http://localhost:3000
export METABASE_USERNAME=you@example.invalid METABASE_PASSWORD=your-password

python3 scripts/verify-metabase.py ./target/release/mcp-bi
```

All 28 checks pass against real Metabase content: a 36-chart dashboard, 8 datasets, live
rows from a saved question, real SQL, and a 27 KB PNG drawn from those rows. It passes
both with credentials and with a supplied `METABASE_TOKEN`.

Two things live testing corrected here, and a reading of the docs would not:

| Assumption | Reality |
|---|---|
| A dataset id can be used as the database to query | A Metabase dataset is a **table**, and a query runs against a *database*. Passing the table id through as `database` reached Metabase's driver code with an id from the wrong space and produced `500 Assert failed: (keyword? driver)`. The table's own `db_id` has to be resolved first |
| A session token is enough | A Metabase session id is an opaque UUID with no readable expiry, so a lapse cannot be anticipated the way Superset's JWT `exp` can. The only reliable signal is the platform refusing it — `401` with the bare body `Unauthenticated`, which is not JSON. Recovery has to be reactive, so the server re-authenticates and retries once |

Both are pinned by fixture tests, so a regression is caught without Metabase running.

## Verified against a live Power BI tenant

The first commercial backend proven rather than reviewed.

```sh
export POWERBI_TOKEN=$(python3 scripts/powerbi-token.py)   # device code, no app registration
python3 scripts/verify-powerbi.py ./target/release/mcp-bi
```

All 32 checks pass against a real tenant: a dashboard with 14 named tiles, a report with 5
pages, a 19-column semantic model, and real DAX. `scripts/powerbi-token.py` signs in by
device code against a Microsoft first-party public client that is already pre-authorised
for the Power BI API, so it needs nothing registered in your tenant and grants nothing your
own account cannot already do.

Five things live testing corrected, and a reading of the docs would not:

| Assumption | Reality |
|---|---|
| Reports are what people mean by a dashboard | Power BI has both, and only listing reports hid the artefact its owner called their dashboard. The report's pages were named "Page 1" to "Page 5"; the dashboard beside it held **14 tiles** named "No. of Houses with Water", "Satisfied with Water Services", "Responses". `bi_list_dashboards` now returns both, labelled, and `bi_get_dashboard` returns tiles or pages depending on which the id names |
| `INFO.COLUMNS()` reads a model's schema | `HTTP 400 DatasetExecuteQueriesError — "Failed to execute the DAX query."` with error code 3239575574 and no further explanation. `INFO.VIEW.COLUMNS()` works and additionally reports `IsHidden`, which matters: a real model carried 9 hidden columns out of 28, including two `RowNumber-…` internals and a hidden date table |
| A DAX result's columns arrive in the order the query asked for | They arrive as a JSON object, and with `serde_json`'s default map they came back **alphabetically** — `SELECTCOLUMNS(… "name" … "kind" … "hidden" …)` returned `["[hidden]", "[kind]", "[name]"]`. Positional access therefore read the wrong field. Fixed at both ends: aliases are looked up by name, and `preserve_order` keeps the query's order |
| `webUrl` is the page to open | It means two different things under one name. `GET /dashboards` reports `…/groups/me/dashboards/{id}`, a page; `GET /dashboards/{id}` reports `…/dashboardEmbed?dashboardId=…&config=…`, a chrome-less embed surface for hosting inside another application |
| Export produces an image given the right permissions | `403 InvalidRequest — "Export report to image is disabled on tenant level"`. An administrator setting, not a permission on the account, so no retry or grant changes it. The refusal now names the setting and points at `bi_dashboard_url` |

What Power BI genuinely cannot do: **its REST API exposes no endpoint returning the data
behind a tile or a page.** `bi_chart_data`, `bi_insights`, `bi_drill_down` and
`bi_render_chart` therefore decline, and the verification script asserts those refusals
alongside the successes — a backend that improvised numbers here would be worse than one
that declines. `bi_query` with DAX is the route that works, and it works well: 45 survey
responses grouped by block, by answer and by satisfaction, straight out of the model.

## Verified against a live Qlik Cloud tenant

```sh
export QLIK_URL=https://your-tenant.eu.qlikcloud.com
export QLIK_TOKEN=your-api-key      # Profile settings ▸ API keys ▸ Generate new key
python3 scripts/verify-qlik.py ./target/release/mcp-bi
```

All 29 checks pass against a trial tenant: an app's three business tables with row counts
of 632,313, 245 and 30, and a 14-field schema with types.

This adapter refused **five of its seven methods**, all on one claim: that "Qlik apps are
read through the Engine JSON API over a WebSocket, not REST". Live testing split that claim
in half.

| Assumption | Reality |
|---|---|
| Listing datasets is not possible over REST | `GET /api/v1/apps/{id}/data/metadata` returns the app's tables with row counts, and every field with its type tags. `bi_list_datasets` and `bi_describe_dataset` now work; the refusals were wrong rather than cautious |
| Sheets and visuals are not possible over REST | **True, and now measured rather than assumed.** `/apps/{id}/objects` and `/apps/{id}/sheets` both answer `404`. `bi_chart_data`, `bi_drill_down` and `bi_insights` decline, and the verification asserts those refusals |
| An app's title is its id | It presented itself as `a bare GUID`, the same defect the Power BI adapter had. `attributes.name` is one REST call away |
| A numeric field is a measure, so not groupable | Not in Qlik. Its associative model makes **any** field selectable as a dimension, and the metadata carries nothing to infer a measure from: `Date_year` has 2 distinct values and `Quantity` has 3, so cardinality cannot separate them and there is no `SummarizeBy` equivalent to read. Marking `Date_year` ungroupable would stop an agent grouping by year on a sales model, so every field is reported groupable and `kind` carries the numeric signal instead |

Two smaller shapes worth knowing: a dataset id is `{appId}:{tableName}`, because Qlik has no
single object for a queryable table — an app holds tables, while `/items?resourceType=dataset`
lists `.qvd` and `.txt` *files* with no schema endpoint. And Qlik's own bookkeeping is
filtered out: tables flagged `is_system` (`$$SysTable 3`) and fields tagged `$system`
(`$Field`, `$Table`, `$Rows`) describe the model rather than the business.

## Not yet verified

Three commercial adapters — Tableau, Looker and QuickSight — are mapped from published API
references and have **never run against a real tenant**. Treat their endpoint shapes as
reviewed, not proven.

Take that literally. Live testing has now corrected three assumptions in Superset, two in
Metabase, five in Power BI and four in Qlik. In every case the wrong version looked
reasonable against the documentation, and twice the failure was **silent**: Power BI
returned HTTP 200 with an empty column list, and Qlik declined work it was capable of.

If you have a tenant, the four verify scripts show the shape a check takes, and a report of
what it finds is welcome.
