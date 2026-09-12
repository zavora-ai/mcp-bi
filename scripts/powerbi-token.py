#!/usr/bin/env python3
"""Obtain a Power BI access token by device code, and print it.

Power BI's REST API takes an Entra ID bearer token. Getting one is the part that stops
people verifying the adapter, so this does it with no app registration and no admin
consent: a device code flow against a Microsoft first-party public client that is already
pre-authorised for the Power BI API.

    export POWERBI_TOKEN=$(python3 scripts/powerbi-token.py)
    BI_BACKEND=powerbi python3 scripts/verify-powerbi.py ./target/release/mcp-bi

The secret never leaves your browser — this process only ever sees the resulting token.
The token is short-lived (about an hour) and read-only for the scopes your own account
already has; this grants nothing your user cannot already do in the portal.

Pass --refresh-token to print the refresh token instead, if you want a longer session.
"""
import json
import sys
import time
import urllib.error
import urllib.parse
import urllib.request

# Azure CLI's public client id. A Microsoft first-party public client, pre-authorised for
# the Power BI API, which is why this needs nothing registered in your tenant.
CLIENT_ID = "04b07795-8ddb-461a-bbee-02f9e1bf7b46"
SCOPE = "https://analysis.windows.net/powerbi/api/.default offline_access"
AUTHORITY = "https://login.microsoftonline.com/common/oauth2/v2.0"


def post(path, fields):
    body = urllib.parse.urlencode(fields).encode()
    request = urllib.request.Request(f"{AUTHORITY}/{path}", data=body)
    try:
        with urllib.request.urlopen(request) as response:
            return json.load(response), None
    except urllib.error.HTTPError as error:
        return None, json.load(error)


def main():
    started, failure = post("devicecode", {"client_id": CLIENT_ID, "scope": SCOPE})
    if failure:
        print(f"could not start sign-in: {failure.get('error_description', failure)}", file=sys.stderr)
        return 1

    # To stderr, so `export TOKEN=$(...)` captures only the token.
    print(f"\nOpen {started['verification_uri']} and enter code: {started['user_code']}\n",
          file=sys.stderr)

    deadline = time.time() + int(started.get("expires_in", 900))
    interval = int(started.get("interval", 5))
    while time.time() < deadline:
        token, failure = post("token", {
            "grant_type": "urn:ietf:params:oauth:grant-type:device_code",
            "client_id": CLIENT_ID,
            "device_code": started["device_code"],
        })
        if token:
            wanted = "refresh_token" if "--refresh-token" in sys.argv else "access_token"
            value = token.get(wanted)
            if not value:
                print(f"sign-in succeeded but returned no {wanted}", file=sys.stderr)
                return 1
            print(f"signed in; token valid for about {token.get('expires_in', '?')} seconds",
                  file=sys.stderr)
            print(value)
            return 0
        code = failure.get("error")
        if code == "authorization_pending":
            time.sleep(interval)
            continue
        if code == "slow_down":
            interval += 5
            time.sleep(interval)
            continue
        print(f"sign-in stopped: {code} — {failure.get('error_description', '')[:200]}",
              file=sys.stderr)
        return 1

    print("sign-in timed out", file=sys.stderr)
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
