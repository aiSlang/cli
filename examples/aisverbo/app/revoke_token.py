#!/usr/bin/env python3
"""Revoke an aisVerbo access JWT — by the token itself or its jti.

    python3 app/revoke_token.py --token eyJ...
    python3 app/revoke_token.py --jti 3f2a...-...

The revocation list lives in the default database (shared across every
tenant); the hosted equivalent is POST /api/admin/revoke {token|jti} with
a corpus_admin token.
"""
import argparse
import datetime
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import auth     # noqa: E402
import db as dbmod  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description="Revoke an aisVerbo JWT.")
    ap.add_argument("--token", help="the full JWT to revoke")
    ap.add_argument("--jti", help="or just its jti claim")
    args = ap.parse_args()
    jti = args.jti
    if args.token:
        payload = auth.verify(args.token) or {}
        jti = payload.get("jti") or jti
        if not jti:
            print("token has no jti (pre-revocation mint) — it can only "
                  "expire; rotate AISVERBO_SECRET to kill it now")
            return 2
    if not jti:
        print("give --token or --jti")
        return 2
    now = datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")
    dbmod.revoke_jti(jti, now)
    print(f"revoked jti {jti}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
