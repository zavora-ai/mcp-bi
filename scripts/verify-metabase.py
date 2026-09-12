#!/usr/bin/env python3
"""Exercise mcp-bi against a live Metabase, over real MCP stdio.

Proves the second open-source backend against the platform itself rather than against
recorded fixtures. Reads METABASE_URL and either METABASE_USERNAME/METABASE_PASSWORD
or METABASE_TOKEN from the environment.

Prefer the credentials. A Metabase session id is an opaque UUID that carries no
expiry, so the server cannot tell in advance that one has lapsed — with credentials it
obtains a new session when the platform rejects the old one, and without them a run
that outlives its session fails partway through.
"""
import base64
import json
import os
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/release/mcp-bi"

env = dict(os.environ, BI_BACKEND="metabase")
proc = subprocess.Popen(
    [BINARY], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
    stderr=subprocess.DEVNULL, text=True, bufsize=1, env=env,
)
sequence = 0


def call(method, params):
    global sequence
    sequence += 1
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": sequence, "method": method, "params": params}) + "\n")
    proc.stdin.flush()
    return json.loads(proc.stdout.readline())


def notify(method, params):
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": method, "params": params}) + "\n")
    proc.stdin.flush()


def tool(name, arguments):
    """Call a tool and return (parsed_json, image_blocks)."""
    result = call("tools/call", {"name": name, "arguments": arguments})["result"]
    blocks = result.get("content", [])
    text = next((b["text"] for b in blocks if b.get("type") == "text"), "{}")
    images = [b for b in blocks if b.get("type") == "image"]
    if result.get("isError"):
        return {"__error__": text}, images
    return json.loads(text), images


failures = []


def check(label, condition, detail=""):
    print(f"  {'PASS' if condition else 'FAIL'}  {label}{(' — ' + str(detail)) if detail else ''}")
    if not condition:
        failures.append(label)


call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "metabase-probe", "version": "1"}})
notify("notifications/initialized", {})

print("\n1. bi_backend_info")
info, _ = tool("bi_backend_info", {})
check("reports metabase", info.get("backend") == "metabase", info.get("backend"))
check("declares itself open source", info.get("open_source") is True)
check("claims chart_data", info["capabilities"]["chart_data"] is True)
check("claims raw_query", info["capabilities"]["raw_query"] is True)
# Metabase renders dashboards in the browser and exposes no server-side image endpoint.
# A backend that claimed otherwise would fail at the point of use instead of here.
check("does not claim dashboard images",
      info["capabilities"].get("dashboard_image") is not True,
      info["capabilities"].get("dashboard_image"))

print("\n2. bi_list_dashboards — real saved dashboards")
listed, _ = tool("bi_list_dashboards", {})
check("found dashboards", listed.get("count", 0) > 0, listed.get("count"))
titles = [d["title"] for d in listed.get("dashboards", [])]
print("      " + ", ".join(titles[:6]))
check("titles are populated", all(t and t != "(untitled)" for t in titles))
check("URLs are absolute", all((d.get("url") or "").startswith("http") for d in listed["dashboards"]))

print("\n3. bi_get_dashboard — charts and their drillable dimensions")
chosen, chart = None, None
for dashboard in listed["dashboards"]:
    opened, _ = tool("bi_get_dashboard", {"dashboard_id": dashboard["id"]})
    for candidate in opened.get("charts", []):
        if candidate.get("id"):
            chosen, chart = opened, candidate
            break
    if chart:
        break
check("a dashboard exposes charts", chart is not None)
if chart:
    print(f"      dashboard {chosen.get('id')}: {len(chosen['charts'])} charts, using {chart['id']}")
    check("chart names its dataset", bool(chart.get("dataset_id")), chart.get("dataset_id"))

print("\n4. bi_list_datasets — Metabase tables, whose ids are not database ids")
datasets, _ = tool("bi_list_datasets", {})
check("found datasets", datasets.get("count", 0) > 0, datasets.get("count"))
first = datasets["datasets"][0] if datasets.get("datasets") else {}
check("datasets carry ids", bool(first.get("id")), first.get("id"))

print("\n5. bi_describe_dataset — columns with kinds")
if first.get("id"):
    described, _ = tool("bi_describe_dataset", {"dataset_id": first["id"]})
    columns = described.get("columns", [])
    check("columns are reported", len(columns) > 0, len(columns))
    check("columns carry a kind", all(c.get("kind") for c in columns))

print("\n6. bi_query — real SQL, resolving the table's own database")
# The bug this pins: a dataset id is a *table* id, and a query runs against a database.
# Passing the table id through as the database produced a 500 from Metabase's own
# driver code — a valid id in the wrong id space.
if first.get("id"):
    queried, _ = tool("bi_query", {"dataset_id": first["id"], "query": "SELECT 1 AS probe", "limit": 3})
    if queried.get("__error__"):
        check("SQL ran", False, queried["__error__"][:200])
    else:
        check("SQL ran", queried.get("rows", 0) >= 1, f"{queried.get('rows')} row(s)")
        check("columns came back", queried["table"]["columns"] == ["probe"], queried["table"]["columns"])

print("\n7. bi_chart_data — live rows from a saved question")
if chart:
    data, _ = tool("bi_chart_data", {"chart_id": chart["id"]})
    if data.get("__error__"):
        check("chart returned data", False, data["__error__"][:200])
    else:
        check("chart returned rows", data.get("rows", 0) > 0, data.get("rows"))
        check("columns are named", len(data["table"]["columns"]) > 0, data["table"]["columns"][:3])

print("\n8. bi_insights — statistics computed from those rows")
if chart:
    insights, _ = tool("bi_insights", {"chart_id": chart["id"]})
    found = insights.get("insights", [])
    check("insights were produced", len(found) > 0, len(found))
    if found:
        print(f"      e.g. {found[0].get('column')}: change {found[0].get('change_pct')}%")

print("\n9. bi_drill_down — a narrower view of the same chart")
if chart:
    drilled, _ = tool("bi_drill_down", {"chart_id": chart["id"]})
    drill = drilled.get("drill", {})
    check("drill reports both sides", "rows_before" in drill and "rows_after" in drill,
          f"{drill.get('rows_before')} → {drill.get('rows_after')}")

print("\n10. bi_render_chart — a PNG drawn from live rows")
if chart:
    rendered, images = tool("bi_render_chart", {"chart_id": chart["id"], "width": 900, "height": 500})
    if rendered.get("__error__"):
        check("rendered a chart", False, rendered["__error__"][:200])
    else:
        check("returned an image block", len(images) == 1, len(images))
        if images:
            raw = base64.b64decode(images[0]["data"])
            check("image is a real PNG", raw[:8] == b"\x89PNG\r\n\x1a\n")
            print(f"      {len(raw)} bytes, from {rendered.get('rows', '?')} rows of live Metabase data")

print("\n11. bi_export_dashboard_image — a capability Metabase does not have")
# Asserting the *refusal*. Metabase renders dashboards client-side, so the honest
# answer is a structured error naming the limit, not a blank or invented image.
if listed.get("dashboards"):
    exported, _ = tool("bi_export_dashboard_image", {"dashboard_id": listed["dashboards"][0]["id"]})
    refused = exported.get("__error__", "")
    check("refuses rather than inventing an image", bool(refused), refused[:90])
    check("the refusal names the platform", "metabase" in refused.lower())

print("\n12. memory — corrections survive, and only what was asked for")
subject = "metabase-verification-probe"
tool("bi_remember", {"subject": subject, "note": "dataset ids are table ids; the db_id differs"})
recalled, _ = tool("bi_recall", {"question": "are dataset ids the same as database ids"})
check("a stored correction is recalled", recalled.get("matched", 0) > 0, recalled.get("matched"))
forgotten, _ = tool("bi_forget", {"subject": subject})
check("and can be removed", forgotten.get("removed", 0) > 0, forgotten.get("removed"))

print("\n13. read-only posture")
tools = call("tools/list", {})["result"]["tools"]
writing = [t["name"] for t in tools
           if any(word in t["name"] for word in ("create", "update", "delete", "patch", "write"))
           and not t["name"].startswith("bi_forget")]
check("no tool writes to the platform", not writing, writing or "none")
check("all 15 tools are advertised", len(tools) == 15, len(tools))

proc.terminate()
print(f"\n{'=' * 60}")
if failures:
    print(f"{len(failures)} check(s) failed: {failures}")
    raise SystemExit(1)
print("every check passed against live Metabase")
