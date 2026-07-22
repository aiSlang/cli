"""Offline tests for the Fetcher parsers + driver (scripts/fetch.py).

Network calls are never made: the source adapters are monkeypatched, so
these test the parsing/normalization and the fetch_report assembly only.

Run:  python3 -m pytest examples/portfolio-pnl/tests -q
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "scripts"))
import db as dbmod    # noqa: E402
import fetch as ft    # noqa: E402


def test_parse_stooq_csv():
    text = ("Date,Open,High,Low,Close,Volume\n"
            "2024-06-03,190.0,192.0,189.0,191.5,1000\n"
            "2024-06-04,191.5,193.0,191.0,192.25,1200\n"
            "2024-06-05,192.25,192.5,,N/D,0\n")          # bad row skipped
    rows = ft.parse_stooq_csv(text)
    assert rows == [("2024-06-03", 191.5), ("2024-06-04", 192.25)]


def test_stooq_symbol_mapping_and_override():
    assert ft.stooq_symbol({"symbol": "AAPL"}) == "aapl.us"
    assert ft.stooq_symbol({"symbol": "BRK-B"}) == "brk.us"       # suffix stripped
    assert ft.stooq_symbol({"symbol": "X", "stooq_symbol": "x.uk"}) == "x.uk"


def test_parse_yahoo():
    payload = {"chart": {"result": [{
        "timestamp": [1704067200, 1704153600, 1704240000],  # 3 UTC days
        "indicators": {"quote": [{"close": [187.0, None, 189.5]}]},  # null gap skipped
    }]}}
    rows = ft.parse_yahoo(payload)
    assert [r[1] for r in rows] == [187.0, 189.5]
    assert all(len(r[0]) == 10 for r in rows)   # ISO YYYY-MM-DD


def test_parse_coingecko_dedups_to_daily():
    payload = {"prices": [
        [1700000000000, 35000.0],
        [1700086400000, 36000.0],
        [1700086500000, 36100.0],   # same UTC day -> last wins
    ]}
    rows = ft.parse_coingecko(payload)
    assert len(rows) == 2
    assert rows[-1][1] == 36100.0


def test_coingecko_id_mapping_and_override():
    assert ft.coingecko_id({"symbol": "BTC-USD"}) == "bitcoin"
    assert ft.coingecko_id({"symbol": "WHATEVER", "cg_id": "my-coin"}) == "my-coin"


def test_fetch_all_dispatch(tmp_path, monkeypatch):
    # Fake the network adapter so no HTTP happens.
    monkeypatch.setitem(ft.ADAPTERS, "yahoo",
                        lambda p, days: [("2024-01-01", 100.0), ("2024-01-02", 101.0)])
    holdings = {
        "benchmark": "SPY",                                                # auto-added, yahoo
        "positions": [
            {"symbol": "AAPL", "asset_class": "equity", "source": "yahoo"},
            {"symbol": "BTC-USD", "asset_class": "crypto", "source": "tradingview"},  # -> pending
            {"symbol": "XYZ", "asset_class": "equity", "source": "nope"},             # -> failure
        ],
    }
    conn = dbmod.connect(str(tmp_path / "t.db"))
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, {"positions": [
        {"symbol": s["symbol"], "asset_class": s["asset_class"], "source": s.get("source"),
         "lots": [{"quantity": 1, "cost_basis": 1.0, "date": "2024-01-01"}]}
        for s in holdings["positions"]]})

    report = ft.fetch_all(conn, holdings, days=180)
    got = {f["symbol"] for f in report["fetched"]}
    assert "AAPL" in got and "SPY" in got                 # SPY (benchmark) auto-added, stooq
    assert report["pending"][0]["symbol"] == "BTC-USD"
    assert report["failures"][0]["symbol"] == "XYZ"
    assert report["summary"] == {"ok": 2, "failed": 1, "pending": 1}
    # prices actually landed in the DB
    assert len(dbmod.price_history(conn, "AAPL")) == 2


def test_fetch_all_empty_source_is_failure(tmp_path, monkeypatch):
    monkeypatch.setitem(ft.ADAPTERS, "yahoo", lambda p, days: [])
    holdings = {"positions": [{"symbol": "AAPL", "asset_class": "equity", "source": "yahoo"}]}
    conn = dbmod.connect(str(tmp_path / "t.db"))
    dbmod.init_db(conn)
    dbmod.sync_holdings(conn, {"positions": [
        {"symbol": "AAPL", "asset_class": "equity", "source": "stooq",
         "lots": [{"quantity": 1, "cost_basis": 1.0}]}]})
    report = ft.fetch_all(conn, holdings, days=180)
    assert report["failures"][0]["error"] == "no data returned"
