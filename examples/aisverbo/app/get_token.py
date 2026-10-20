#!/usr/bin/env python3
"""Mint an aisVerbo JWT — by default the ADMIN token that unlocks corpus
management (POST /api/verbs, POST /api/tenses).

    python3 app/get_token.py                     # admin, valid 365 days
    python3 app/get_token.py --days 30 --sub me
    AISVERBO_SECRET=... python3 app/get_token.py # sign with the Space secret

Paste the printed token into Setup → "Admin token" (stored only in that
browser). The signing secret must match the server's: the AISVERBO_SECRET
env var, or the auto-generated <data dir>/secret.key.
"""
import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import auth  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser(description="Mint an aisVerbo access JWT.")
    ap.add_argument("--role", default="corpus_admin",
                    choices=["corpus_admin", "story_writer"],
                    help="corpus_admin: manage verbs/tenses; story_writer: "
                         "generate and delete stories (and LLM extras)")
    ap.add_argument("--sub", default="public",
                    help="WHO this token belongs to: the HF username the "
                         "holder signs in with, or 'public' for the shared "
                         "profile (a sub-bound token is refused for anyone "
                         "else — the HF sign-in IS the verification)")
    ap.add_argument("--days", type=float, default=365,
                    help="validity in days (0 = no expiry)")
    args = ap.parse_args()
    import uuid
    jti = str(uuid.uuid4())
    payload = {"role": args.role, "sub": args.sub, "jti": jti,
               "iat": int(time.time())}
    if args.days:
        payload["exp"] = int(time.time() + args.days * 86400)
    print(auth.sign(payload))
    print(f"# jti (keep for revocation): {jti}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
