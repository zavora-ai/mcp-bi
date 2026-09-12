#!/usr/bin/env python3
"""Exercise mcp-bi against a live Power BI tenant, over real MCP stdio.

Reads POWERBI_TOKEN, and optionally POWERBI_GROUP_ID for a workspace other than My
Workspace. `scripts/powerbi-token.py` will obtain a token by device code.

Unlike the Superset and Metabase scripts, this one asserts several *refusals*. Power BI's
REST API does not expose the data behind a visual — no endpoint returns a tile's or a
page's rows — so the tools that need that must say so rather than return something
plausible. A backend that invented numbers here would be worse than one that declines.
"""
import base64
import json
import os
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/release/mcp-bi"

if not os.environ.get("POWERBI_TOKEN"):
    print("POWERBI_TOKEN is not set. Run: export POWERBI_TOKEN=$(python3 scripts/powerbi-token.py)")
    raise SystemExit(2)

env = dict(os.environ, BI_BACKEND="powerbi")
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
                    "clientInfo": {"name": "powerbi-probe", "version": "1"}})
notify("notifications/initialized", {})

print("\n1. bi_backend_info")
info, _ = tool("bi_backend_info", {})
check("reports powerbi", info.get("backend") == "powerbi", info.get("backend"))
check("does not claim to be open source", info.get("open_source") is False)
check("claims raw_query (DAX)", info["capabilities"]["raw_query"] is True)
# The honest half. Power BI exposes no per-visual data endpoint, so claiming chart_data
# would promise something no request can deliver.
check("does not claim chart_data", info["capabilities"]["chart_data"] is False)

print("\n2. bi_list_dashboards — both artefacts, because Power BI has two")
listed, _ = tool("bi_list_dashboards", {})
check("found something", listed.get("count", 0) > 0, listed.get("count"))
kinds = [d.get("description") or "" for d in listed.get("dashboards", [])]
check("dashboards are labelled as such", any("pinned tiles" in k for k in kinds))
check("reports are labelled as such", any("pages of visuals" in k for k in kinds))
for d in listed.get("dashboards", [])[:6]:
    print(f"      {d['title'][:34]:<36} {(d.get('description') or '')[:34]}")
check("titles are populated",
      all(d["title"] and d["title"] != "(untitled)" for d in listed.get("dashboards", [])))

print("\n3. bi_get_dashboard — tiles for a dashboard, pages for a report")
tiles_seen, pages_seen, tile_dataset = 0, 0, None
for d in listed.get("dashboards", []):
    opened, _ = tool("bi_get_dashboard", {"dashboard_id": d["id"]})
    charts = opened.get("charts", [])
    kinds = {c.get("kind") for c in charts}
    if "tile" in kinds:
        tiles_seen = len(charts)
        tile_dataset = next((c.get("dataset_id") for c in charts if c.get("dataset_id")), None)
        print(f"      dashboard {opened['title'][:22]!r}: {len(charts)} tiles, e.g. "
              f"{[c['title'][:24] for c in charts[:3]]}")
    if "page" in kinds:
        pages_seen = len(charts)
        print(f"      report    {opened['title'][:22]!r}: {len(charts)} pages")
check("a dashboard's tiles are returned", tiles_seen > 0, tiles_seen)
check("tiles name their dataset", bool(tile_dataset), tile_dataset)
# Not fatal: a tenant may have reports and no dashboards, or the reverse.
if pages_seen:
    check("a report's pages are returned", pages_seen > 0, pages_seen)

print("\n4. bi_list_datasets")
datasets, _ = tool("bi_list_datasets", {})
check("found datasets", datasets.get("count", 0) > 0, datasets.get("count"))
first = datasets["datasets"][0] if datasets.get("datasets") else {}
check("datasets are named, not just identified",
      bool(first.get("name")) and first.get("name") != first.get("id"), first.get("name"))

print("\n5. bi_describe_dataset — the model's shape, via DAX over its metadata")
# The defect this pins: `name` was filled with the id, so every model described itself as
# a GUID; and the metadata query used INFO.COLUMNS(), which answers HTTP 400 on a live
# tenant, with the failure swallowed into an empty column list.
if first.get("id"):
    described, _ = tool("bi_describe_dataset", {"dataset_id": first["id"]})
    if described.get("__error__"):
        check("described the dataset", False, described["__error__"][:200])
    else:
        check("name is the model's name, not its id",
              described.get("name") != described.get("id"), described.get("name"))
        columns = described.get("columns", [])
        check("columns are reported", len(columns) > 0, len(columns))
        check("columns carry a kind", all(c.get("kind") for c in columns))
        # Hidden columns are the model's bookkeeping; a real model carried nine, including
        # two RowNumber internals and a hidden date table.
        check("internal columns are excluded",
              not any("RowNumber-" in c["name"] for c in columns))
        print(f"      {len(columns)} visible columns, e.g. {[c['name'][:20] for c in columns[:4]]}")

print("\n6. bi_query — real DAX")
if first.get("id"):
    queried, _ = tool("bi_query", {"dataset_id": first["id"],
                                   "query": 'EVALUATE ROW("probe", 1)', "limit": 3})
    if queried.get("__error__"):
        check("DAX ran", False, queried["__error__"][:200])
    else:
        check("DAX ran", queried.get("rows", 0) >= 1, f"{queried.get('rows')} row(s)")
        check("the requested column came back", queried["table"]["columns"] == ["[probe]"],
              queried["table"]["columns"])

print("\n7. bi_query — column order follows the query")
# Worth pinning. Result columns arrive as a JSON object, and with serde_json's default
# map they came back alphabetically: a query asking for "a", "b", "c" returned them
# sorted, so any code reading them positionally read the wrong field. That is the kind of
# bug that works in testing and fails on a different query.
if first.get("id"):
    ordered, _ = tool("bi_query", {"dataset_id": first["id"],
                                   "query": 'EVALUATE ROW("zebra", 1, "apple", 2)', "limit": 2})
    if not ordered.get("__error__"):
        check("columns are in query order, not sorted",
              ordered["table"]["columns"] == ["[zebra]", "[apple]"],
              ordered["table"]["columns"])

print("\n8. refusals — what the REST API cannot do")
# Each of these must decline rather than improvise. Power BI publishes no endpoint that
# returns the rows behind a tile or a page.
if tiles_seen:
    opened, _ = tool("bi_get_dashboard", {"dashboard_id": listed["dashboards"][0]["id"]})
    chart_id = opened["charts"][0]["id"]
    for name in ("bi_chart_data", "bi_insights", "bi_drill_down", "bi_render_chart"):
        answer, _ = tool(name, {"chart_id": chart_id})
        refused = answer.get("__error__", "")
        check(f"{name} declines rather than inventing data", bool(refused),
              refused[:64].replace("\n", " "))

print("\n9. bi_export_dashboard_image — a tenant may forbid it outright")
if listed.get("dashboards"):
    exported, images = tool("bi_export_dashboard_image",
                            {"dashboard_id": listed["dashboards"][-1]["id"]})
    error = exported.get("__error__", "")
    if "switched off for this tenant" in error:
        # Measured: 403 InvalidRequest, "Export report to image is disabled on tenant
        # level". No retry and no permission on this account changes it, so the message
        # has to name the administrator setting instead of repeating the status.
        check("a tenant-level block is explained, not just reported", True, "tenant policy")
        check("and it names the alternative", "bi_dashboard_url" in error)
    elif error:
        check("an async export is explained", "poll" in error.lower() or "capacity" in error.lower(),
              error[:90])
    else:
        check("returned an image", len(images) == 1)
        if images:
            raw = base64.b64decode(images[0]["data"])
            check("image is a real PNG", raw[:8] == b"\x89PNG\r\n\x1a\n")

print("\n10. bi_dashboard_url — a link that matches the artefact")
# "starts with https" is not enough. `reportEmbed?reportId=` built from a *dashboard* id
# is a well-formed URL for a report that does not exist, so the link has to name the kind
# of thing the id actually is.
for d in listed.get("dashboards", []):
    linked, _ = tool("bi_dashboard_url", {"dashboard_id": d["id"]})
    url = linked.get("url") or ""
    is_dashboard = "pinned tiles" in (d.get("description") or "")
    expected = "dashboard" if is_dashboard else "report"
    check(f"{expected} link points at a {expected}", expected in url.lower(), url[:78])
    # And it must be a page, not an embed surface: an embed URL renders chrome-less
    # content meant for hosting inside another application.
    check(f"{expected} link is a portal page, not an embed", "embed" not in url.lower(), url[:78])

print("\n11. read-only posture")
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
print("every check passed against live Power BI")
