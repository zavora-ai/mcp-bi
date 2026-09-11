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
  SUPERSET_TOKEN=... cargo run
```

| Backend | Variables |
|---|---|
| `superset` | `SUPERSET_URL`, `SUPERSET_TOKEN` |
| `metabase` | `METABASE_URL`, `METABASE_TOKEN` |
| `powerbi` | `POWERBI_TOKEN`, optional `POWERBI_GROUP_ID`, `POWERBI_API` |
| `tableau` | `TABLEAU_URL`, `TABLEAU_TOKEN`, `TABLEAU_SITE_ID` |
| `looker` | `LOOKER_URL`, `LOOKER_TOKEN` |
| `qlik` | `QLIK_URL`, `QLIK_TOKEN` |
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
export SUPERSET_TOKEN=$(curl -s -X POST $SUPERSET_URL/api/v1/security/login \
  -H 'Content-Type: application/json' \
  -d '{"username":"admin","password":"admin","provider":"db","refresh":true}' \
  | python3 -c 'import json,sys;print(json.load(sys.stdin)["access_token"])')

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

## Not yet verified

Metabase is tested against recorded responses following its documented API, not
against a live instance. The five commercial adapters are mapped from published API
references and have never run against a real tenant — treat their endpoint shapes as
reviewed, not proven. Given what live testing corrected in Superset, expect at least
one surprise per platform.
