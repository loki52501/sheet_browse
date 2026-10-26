"""Fixture Chrome `History` SQLite for Phase 1 ladder rung 3 (docs/08 Q-1, F-6).

Writes a file with Chrome's `urls` and `visits` tables (the columns history.search reads)
holding a few fictional visits, dated relative to now, so "the Rust CDP crate page I looked
at last week" has exactly one right answer and some distractors. Never reads or copies the
user's real Chrome history.

    python -I bench/fixtures/chrome_history_fixture.py <out-path>

Point SB_CHROME_HISTORY at <out-path> for the session.
"""

import datetime as dt
import os
import sqlite3
import sys

EPOCH_GAP_SECS = 11_644_473_600  # 1601-01-01 to 1970-01-01

# (url, title, days ago). The answer: chromiumoxide on crates.io, 6 days ago.
ROWS = [
    ("https://crates.io/crates/chromiumoxide",
     "chromiumoxide - crates.io: Rust Package Registry", 6),
    ("https://pypi.org/project/pychrome/",
     "pychrome - a Python package for the Chrome DevTools Protocol (cdp)", 5),
    ("https://docs.rs/headless_chrome/latest/headless_chrome/",
     "headless_chrome - Rust", 30),
    ("https://chromedevtools.github.io/devtools-protocol/",
     "Chrome DevTools Protocol", 8),
    ("https://www.rust-lang.org/learn", "Learn Rust - Rust Programming Language", 2),
    ("https://news.ycombinator.com/", "Hacker News", 1),
    ("https://en.wikipedia.org/wiki/Sourdough", "Sourdough - Wikipedia", 6),
]


def chrome_time(t: dt.datetime) -> int:
    return int((t.timestamp() + EPOCH_GAP_SECS) * 1_000_000)


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__)
        return 2
    out = sys.argv[1]
    if os.path.exists(out):
        os.remove(out)
    now = dt.datetime.now(dt.timezone.utc).replace(microsecond=0)
    c = sqlite3.connect(out)
    c.executescript(
        """
        CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR,
            title LONGVARCHAR, visit_count INTEGER DEFAULT 0 NOT NULL,
            typed_count INTEGER DEFAULT 0 NOT NULL, last_visit_time INTEGER NOT NULL,
            hidden INTEGER DEFAULT 0 NOT NULL);
        CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER NOT NULL,
            visit_time INTEGER NOT NULL, from_visit INTEGER,
            transition INTEGER DEFAULT 0 NOT NULL, segment_id INTEGER,
            visit_duration INTEGER DEFAULT 0 NOT NULL);
        """
    )
    for url, title, days in ROWS:
        t = chrome_time(now - dt.timedelta(days=days, hours=3))
        cur = c.execute(
            "INSERT INTO urls(url, title, visit_count, last_visit_time) VALUES (?, ?, 1, ?)",
            (url, title, t),
        )
        c.execute("INSERT INTO visits(url, visit_time) VALUES (?, ?)", (cur.lastrowid, t))
    c.commit()
    c.close()
    print(f"wrote {len(ROWS)} rows to {out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
