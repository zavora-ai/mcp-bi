#!/usr/bin/env python3
"""Exercise mcp-bi against a live Qlik Cloud tenant, over real MCP stdio.

Reads QLIK_URL and QLIK_TOKEN. Generate the token in the tenant under
Profile settings ▸ API keys; it is a bearer credential carrying your own permissions
across the whole tenant, so use the shortest expiry offered and revoke it afterwards.

This script asserts both halves of a split that live testing established. Qlik's REST API
*does* expose an app's data model — the adapter used to refuse that, wrongly — and it
genuinely does *not* expose sheets or the data behind a visual, which must be declined
rather than improvised.
"""
import json
import os
import subprocess
import sys

BINARY = sys.argv[1] if len(sys.argv) > 1 else "./target/release/mcp-bi"

if not os.environ.get("QLIK_TOKEN") or not os.environ.get("QLIK_URL"):
    print("set QLIK_URL and QLIK_TOKEN")
    raise SystemExit(2)

env = dict(os.environ, BI_BACKEND="qlik")
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
    if result.get("isError"):
        return {"__error__": text}
    return json.loads(text)


failures = []


def check(label, condition, detail=""):
    print(f"  {'PASS' if condition else 'FAIL'}  {label}{(' — ' + str(detail)) if detail else ''}")
    if not condition:
        failures.append(label)


call("initialize", {"protocolVersion": "2025-06-18", "capabilities": {},
                    "clientInfo": {"name": "qlik-probe", "version": "1"}})
notify("notifications/initialized", {})

print("\n1. bi_backend_info")
info = tool("bi_backend_info", {})
check("reports qlik", info.get("backend") == "qlik", info.get("backend"))
check("does not claim to be open source", info.get("open_source") is False)
check("does not claim chart_data", info["capabilities"]["chart_data"] is False)
check("does not claim raw_query", info["capabilities"]["raw_query"] is False)
notes = " ".join(info["capabilities"].get("notes", []))
check("says the data model IS available", "data model is available" in notes.lower()
      or "bi_list_datasets reports its tables" in notes)
check("says sheets are NOT", "404" in notes and "WebSocket" in notes)

print("\n2. bi_list_dashboards — apps in the tenant")
listed = tool("bi_list_dashboards", {})
check("found apps", listed.get("count", 0) > 0, listed.get("count"))
check("apps are named, not identified",
      all(d["title"] and d["title"] != "(untitled)" for d in listed.get("dashboards", [])))
for d in listed.get("dashboards", [])[:5]:
    print(f"      {d['title'][:44]}")
app_id = listed["dashboards"][0]["id"] if listed.get("dashboards") else None

print("\n3. bi_get_dashboard — the app's name, and what is reachable")
# The defect this pins: the title used to be the app id, so every app presented itself as
# a GUID. The same mistake the Power BI adapter made.
if app_id:
    opened = tool("bi_get_dashboard", {"dashboard_id": app_id})
    check("title is the app's name, not its id", opened.get("title") != app_id, opened.get("title"))
    check("no charts are claimed", len(opened.get("charts", [])) == 0)
    description = opened.get("description") or ""
    check("the description names what IS reachable",
          "bi_list_datasets" in description or "table(s)" in description, description[:70])

print("\n4. bi_list_datasets — an app's tables, which REST does expose")
# This used to answer "listing datasets over REST is not supported". It is: the app
# metadata endpoint returns the tables, with row counts.
datasets = tool("bi_list_datasets", {})
if datasets.get("__error__"):
    check("listed datasets", False, datasets["__error__"][:170])
else:
    check("listed datasets", datasets.get("count", 0) > 0, datasets.get("count"))
    for d in datasets.get("datasets", [])[:6]:
        print(f"      {d['name'][:46]:<48} rows={d.get('row_count')}")
    check("tables report row counts",
          any(d.get("row_count") for d in datasets.get("datasets", [])))
    # Qlik's own bookkeeping tables are named `$$SysTable N` and describe the model rather
    # than the business.
    check("Qlik's system tables are excluded",
          not any("SysTable" in d["name"] for d in datasets.get("datasets", [])))
    check("ids name both the app and the table",
          all(":" in d["id"] for d in datasets.get("datasets", [])))

print("\n5. bi_describe_dataset — a table's fields and their types")
first = (datasets.get("datasets") or [{}])[0]
if first.get("id"):
    described = tool("bi_describe_dataset", {"dataset_id": first["id"]})
    if described.get("__error__"):
        check("described the table", False, described["__error__"][:170])
    else:
        columns = described.get("columns", [])
        check("columns are reported", len(columns) > 0, len(columns))
        check("columns carry a kind", all(c.get("kind") for c in columns))
        kinds = {c["kind"] for c in columns}
        check("types are distinguished, not all strings", len(kinds) > 1, sorted(kinds))
        # Qlik's internal model fields — $Field, $Table, $Rows — exist in every app and
        # describe nothing about the data.
        check("Qlik's internal fields are excluded",
              not any(c["name"].startswith("$") for c in columns))
        check("row count is reported", described.get("row_count") is not None,
              described.get("row_count"))
        print(f"      {len(columns)} fields, e.g. "
              f"{[(c['name'][:16], c['kind']) for c in columns[:4]]}")

print("\n6. bi_describe_dataset — a table that does not exist")
# An empty column list would read as a table with no fields, so a wrong name has to be
# refused by name, listing what is actually there.
if app_id:
    missing = tool("bi_describe_dataset", {"dataset_id": f"{app_id}:NoSuchTable"})
    error = missing.get("__error__", "")
    check("a wrong table name is refused", bool(error), error[:60])
    check("and the refusal lists the real tables", "holds" in error.lower(), error[:110])

print("\n7. refusals — what Qlik's REST API genuinely cannot do")
# Measured, not assumed: /apps/{id}/objects and /apps/{id}/sheets both answer 404.
for name, args in [
    ("bi_chart_data", {"chart_id": "any"}),
    ("bi_drill_down", {"chart_id": "any"}),
    ("bi_insights", {"chart_id": "any"}),
]:
    answer = tool(name, args)
    check(f"{name} declines rather than inventing data", bool(answer.get("__error__")),
          answer.get("__error__", "")[:56].replace("\n", " "))

print("\n8. bi_dashboard_url — a link into the app")
if app_id:
    linked = tool("bi_dashboard_url", {"dashboard_id": app_id})
    url = linked.get("url") or ""
    check("URL is absolute", url.startswith("https://"), url[:70])
    check("URL points at the app", f"/sense/app/{app_id}" in url)

print("\n9. read-only posture")
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
print("every check passed against live Qlik Cloud")
