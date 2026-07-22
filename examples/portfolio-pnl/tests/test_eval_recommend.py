"""Run the deterministic recommendation golden eval as part of the test suite.

Run:  python3 -m pytest examples/portfolio-pnl/tests -q
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import eval_recommend as ev  # noqa: E402


def test_recommend_golden_eval_all_pass():
    report = ev.run(ev.DEFAULT_CASES, ev.DEFAULT_BUCKETS)
    failed = [r["id"] for r in report["results"] if not r["passed"]]
    assert not failed, f"golden eval misses: {failed}"
    assert report["pass_rate"] == 1.0
    assert report["total"] >= 4


def test_recommend_golden_eval_is_deterministic():
    report = ev.run(ev.DEFAULT_CASES, ev.DEFAULT_BUCKETS)
    assert all(r["deterministic"] for r in report["results"])
