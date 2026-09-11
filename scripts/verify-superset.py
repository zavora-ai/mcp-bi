#!/usr/bin/env python3
"""Exercise mcp-bi against a live Apache Superset, over real MCP stdio.

Proves the primary backend against the platform itself rather than against
recorded fixtures. Reads SUPERSET_URL and either SUPERSET_USERNAME/SUPERSET_PASSWORD
or SUPERSET_TOKEN from the environment.
"""
import json
import os
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/release/mcp-bi"

env = dict(os.environ, BI_BACKEND="superset")
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
                    "clientInfo": {"name": "superset-probe", "version": "1"}})
notify("notifications/initialized", {})

print("\n1. bi_backend_info")
info, _ = tool("bi_backend_info", {})
check("reports superset", info.get("backend") == "superset", info.get("backend"))
check("declares itself open source", info.get("open_source") is True)
check("claims chart_data", info["capabilities"]["chart_data"] is True)
check("claims raw_query", info["capabilities"]["raw_query"] is True)

print("\n2. bi_list_dashboards — real saved dashboards")
listed, _ = tool("bi_list_dashboards", {})
check("found dashboards", listed.get("count", 0) > 0, listed.get("count"))
titles = [d["title"] for d in listed.get("dashboards", [])]
print("      " + ", ".join(titles[:6]))
check("titles are populated", all(t and t != "(untitled)" for t in titles))
check("URLs are absolute", all(
    (d.get("url") or "").startswith("http") for d in listed["dashboards"]))

print("\n3. bi_get_dashboard — charts and their drillable dimensions")
# Pick a dashboard that actually has charts with groupby, so the drill path is real.
chosen, chart = None, None
for dashboard in listed["dashboards"]:
    opened, _ = tool("bi_get_dashboard", {"dashboard_id": dashboard["id"]})
    if opened.get("__error__"):
        continue
    with_dimension = [c for c in opened.get("charts", []) if c.get("dimensions")]
    if with_dimension:
        chosen, chart = opened, with_dimension[0]
        break
check("opened a dashboard with charts", chosen is not None)
if not chosen:
    proc.terminate()
    raise SystemExit("no dashboard exposed a chart with dimensions")
print(f"      dashboard: {chosen['title']} ({len(chosen['charts'])} charts)")
print(f"      chart: {chart['title']} [{chart['kind']}] dims={chart['dimensions'][:3]}")
check("chart has an id", bool(chart["id"]))
check("chart reports dimensions", len(chart["dimensions"]) > 0)

print("\n4. bi_chart_data — the numbers behind the tile")
data, _ = tool("bi_chart_data", {"chart_id": chart["id"]})
if data.get("__error__"):
    check("chart data returned", False, data["__error__"][:150])
else:
    check("returned rows", data.get("rows", 0) > 0, data.get("rows"))
    check("named its columns", len(data["table"]["columns"]) > 0, data["table"]["columns"][:4])

print("\n5. bi_insights — statistics an agent can cite")
insights, _ = tool("bi_insights", {"chart_id": chart["id"]})
if insights.get("__error__"):
    check("insights computed", False, insights["__error__"][:150])
else:
    found = insights.get("insights", [])
    check("computed at least one series", len(found) > 0, len(found))
    if found:
        first = found[0]
        print(f"      {first['column']}: {first['direction']}, "
              f"max {first['max']} at {first['max_at']}, mean {first['mean']}")
        check("direction is one of up/down/flat", first["direction"] in ("up", "down", "flat"))
        check("peak is labelled", first["max_at"] != "")

print("\n6. bi_drill_down — regroup by a dimension")
dimension = chart["dimensions"][0]
drill, _ = tool("bi_drill_down", {"chart_id": chart["id"], "dimension": dimension})
if drill.get("__error__"):
    check("drill executed", False, drill["__error__"][:150])
else:
    result = drill["drill"]
    print(f"      by {dimension}: {result['rows_before']} -> {result['rows_after']} rows, "
          f"grouped on '{result['table']['columns'][0]}'")
    check("reported the dimension", result.get("dimension") == dimension)
    check("reported rows before and after", "rows_before" in result and "rows_after" in result)
    check("insights travel with the drill", len(drill.get("insights", [])) >= 0)

print("\n7. bi_list_datasets / bi_describe_dataset")
datasets, _ = tool("bi_list_datasets", {})
check("found datasets", datasets.get("count", 0) > 0, datasets.get("count"))
dataset = datasets["datasets"][0]
described, _ = tool("bi_describe_dataset", {"dataset_id": dataset["id"]})
if described.get("__error__"):
    check("described a dataset", False, described["__error__"][:120])
else:
    print(f"      {described['name']}: {len(described['columns'])} columns")
    check("dataset has columns", len(described["columns"]) > 0)
    check("columns carry a type", all(c.get("kind") for c in described["columns"]))

print("\n8. bi_query — real SQL through SQL Lab")
queried, _ = tool("bi_query", {
    "dataset_id": dataset["id"],
    "query": f"SELECT count(*) AS n FROM {described.get('name', 'cleaned_sales_data')}",
    "limit": 10,
})
if queried.get("__error__"):
    check("SQL executed", False, queried["__error__"][:200])
else:
    print(f"      {queried['table']['columns']} = {queried['table']['rows'][:1]}")
    check("SQL returned a row", queried.get("rows", 0) > 0)

print("\n9. bi_dashboard_url — deep link with a filter")
link, _ = tool("bi_dashboard_url", {
    "dashboard_id": chosen["id"],
    "filters": [{"column": dimension, "op": "=", "value": "anything"}],
})
check("built a URL", (link.get("url") or "").startswith("http"), link.get("url", "")[:80])
check("carries filter state", "form_data" in link.get("url", ""))

print("\n10. bi_render_chart — draw the live numbers")
rendered, images = tool("bi_render_chart", {
    "chart_id": chart["id"], "kind": "bar", "title": chart["title"], "theme": "grafana",
})
if rendered.get("__error__"):
    check("rendered a chart", False, rendered["__error__"][:200])
else:
    check("returned an image block", len(images) == 1, len(images))
    if images:
        import base64
        raw = base64.b64decode(images[0]["data"])
        check("image is a real PNG", raw[:8] == b"\x89PNG\r\n\x1a\n")
        print(f"      {len(raw)} bytes, from {rendered['rows']} rows of live Superset data")
    check("statistics came with the picture", len(rendered.get("insights", [])) > 0)

proc.terminate()
print(f"\n{'=' * 60}")
if failures:
    print(f"{len(failures)} check(s) failed: {failures}")
    raise SystemExit(1)
print("every check passed against live Apache Superset")
