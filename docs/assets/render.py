#!/usr/bin/env python3
"""Renders the README figures as light and dark SVGs.

    python3 docs/assets/render.py

Writes `<name>.light.svg` and `<name>.dark.svg` next to this script. The
README picks one with `<picture>` and `prefers-color-scheme`, which GitHub
maps to the viewer's theme. Standard library only; edit the data below and
re-run when the numbers change.

Colours: the accent hues are the first three slots of a palette validated
for colour-vision deficiency (blue, orange, aqua; all pairs pass in both
modes). Light aqua is below 3:1 on the surface, so every coloured mark also
carries a text label.
"""

from __future__ import annotations

from pathlib import Path
from xml.sax.saxutils import escape

OUT = Path(__file__).resolve().parent

THEMES = {
    "light": {
        "surface": "#fcfcfb",
        "panel": "#f3f2ee",
        "ink": "#0b0b0b",
        "ink2": "#52514e",
        "muted": "#6f6d68",
        "grid": "#e1e0d9",
        "axis": "#c3c2b7",
        "border": "#dcdbd3",
        "before": "#b9b8b0",
        "blue": "#2a78d6",
        "blue_soft": "#86b6ef",
        "orange": "#eb6834",
        "aqua": "#1baf7a",
    },
    "dark": {
        "surface": "#1a1a19",
        "panel": "#242423",
        "ink": "#f0efec",
        "ink2": "#c3c2b7",
        "muted": "#9a9891",
        "grid": "#2c2c2a",
        "axis": "#4a4a46",
        "border": "#383835",
        "before": "#5c5b56",
        "blue": "#3987e5",
        "blue_soft": "#1c5cab",
        "orange": "#d95926",
        "aqua": "#199e70",
    },
}

FONT = 'system-ui, -apple-system, "Segoe UI", Helvetica, Arial, sans-serif'
MONO = 'ui-monospace, SFMono-Regular, Menlo, Consolas, monospace'


class Svg:
    def __init__(self, w: int, h: int, t: dict, title: str, desc: str):
        self.w, self.h, self.t = w, h, t
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" '
            f'viewBox="0 0 {w} {h}" role="img" aria-labelledby="title desc" '
            f'font-family=\'{FONT}\'>',
            f'<title id="title">{escape(title)}</title>',
            f'<desc id="desc">{escape(desc)}</desc>',
            "<defs>"
            + "".join(
                f'<marker id="arrow-{k}" viewBox="0 0 10 10" refX="9" refY="5" '
                f'markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
                f'<path d="M0,1 L9,5 L0,9 z" fill="{t[k]}"/></marker>'
                for k in ("ink2", "muted", "blue")
            )
            + "</defs>",
            f'<rect x="0.5" y="0.5" width="{w - 1}" height="{h - 1}" rx="12" '
            f'fill="{t["surface"]}" stroke="{t["border"]}"/>',
        ]

    def add(self, s: str) -> None:
        self.parts.append(s)

    def text(self, x, y, s, size=14, fill="ink", weight=400, anchor="start", mono=False, italic=False):
        fam = f" font-family='{MONO}'" if mono else ""
        it = ' font-style="italic"' if italic else ""
        self.add(
            f'<text x="{x}" y="{y}" font-size="{size}" font-weight="{weight}" '
            f'fill="{self.t[fill]}" text-anchor="{anchor}"{fam}{it}>{escape(s)}</text>'
        )

    def rect(self, x, y, w, h, fill="panel", stroke="border", rx=8, dash=False, sw=1, opacity=None):
        d = ' stroke-dasharray="5 4"' if dash else ""
        op = f' fill-opacity="{opacity}"' if opacity is not None else ""
        st = f'stroke="{self.t[stroke]}" stroke-width="{sw}"' if stroke else 'stroke="none"'
        self.add(
            f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{rx}" '
            f'fill="{self.t[fill]}"{op} {st}{d}/>'
        )

    def line(self, x1, y1, x2, y2, stroke="axis", sw=1, dash=False, arrow=None):
        d = ' stroke-dasharray="5 4"' if dash else ""
        m = f' marker-end="url(#arrow-{arrow})"' if arrow else ""
        self.add(
            f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{self.t[stroke]}" '
            f'stroke-width="{sw}" stroke-linecap="round"{d}{m}/>'
        )

    def save(self, name: str, mode: str) -> None:
        self.add("</svg>")
        (OUT / f"{name}.{mode}.svg").write_text("\n".join(self.parts) + "\n")


# --------------------------------------------------------------------------
# 1. Today vs. Atlantis DB
# --------------------------------------------------------------------------


def one_query(mode: str) -> None:
    t = THEMES[mode]
    W, H = 880, 424
    s = Svg(
        W, H, t,
        "Three systems today, one query in Atlantis DB",
        "Today an agent queries a vector store, a search engine and a graph database "
        "over the network, with sync jobs between them. With Atlantis DB one Cypher "
        "query runs vector, BM25 and graph retrieval on one MVCC snapshot inside the "
        "application process.",
    )
    # Left: today.
    lx, top, cw = 24, 24, 404
    s.rect(lx, top, cw, H - 48, fill="surface", stroke="border", rx=10)
    s.text(lx + 20, top + 32, "Today", 17, weight=650)
    s.text(lx + 20, top + 54, "Three systems and the glue between them", 13.5, "ink2")
    # App box.
    ax, ay, aw, ah = lx + 132, top + 78, 140, 40
    s.rect(ax, ay, aw, ah, fill="panel", stroke="axis")
    s.text(ax + aw / 2, ay + 25, "Your agent / app", 14, weight=600, anchor="middle")
    # Three stores.
    names = [("Vector store", "embeddings"), ("Search engine", "keywords"), ("Graph DB", "relations")]
    bw, bh, by = 112, 56, top + 196
    xs = [lx + 20 + i * (bw + 14) for i in range(3)]
    for (n, sub), x in zip(names, xs):
        s.line(ax + aw / 2, ay + ah, x + bw / 2, by - 4, stroke="muted", sw=1.5, arrow="muted")
        s.rect(x, by, bw, bh, fill="panel", stroke="axis")
        s.text(x + bw / 2, by + 24, n, 14, weight=600, anchor="middle")
        s.text(x + bw / 2, by + 43, sub, 12.5, "ink2", anchor="middle")
    s.text(lx + 292, top + 150, "3 network hops", 12.5, "muted")
    # Sync jobs.
    sy = by + bh + 30
    s.add(
        f'<path d="M{xs[0] + bw / 2},{by + bh} V{sy} H{xs[2] + bw / 2} V{by + bh}" '
        f'fill="none" stroke="{t["muted"]}" stroke-width="1.5" stroke-dasharray="5 4"/>'
    )
    s.line(xs[1] + bw / 2, by + bh, xs[1] + bw / 2, sy, stroke="muted", sw=1.5, dash=True)
    s.rect(lx + cw / 2 - 48, sy - 11, 96, 22, fill="surface", stroke=None, rx=4)
    s.text(lx + cw / 2, sy + 4.5, "sync jobs", 12.5, "ink2", anchor="middle", italic=True)
    for i, line in enumerate(
        ["3 queries, 3 consistency models, 3 permission models.",
         "Search can't see the write the agent just made."]
    ):
        s.text(lx + 20, H - 70 + i * 20, line, 13, "ink2")

    # Right: Atlantis.
    rx_ = lx + cw + 24
    s.rect(rx_, top, cw, H - 48, fill="surface", stroke="blue", rx=10, sw=1.5)
    s.text(rx_ + 20, top + 32, "With Atlantis DB", 17, weight=650)
    s.text(rx_ + 20, top + 54, "One query, one snapshot, inside your process", 13.5, "ink2")
    px, py, pw, ph = rx_ + 20, top + 72, cw - 40, 236
    s.rect(px, py, pw, ph, fill="panel", stroke="axis", dash=True)
    s.text(px + 14, py + 22, "Your process", 12.5, "muted")
    ix, iy, iw, ih = px + 14, py + 34, pw - 28, ph - 48
    s.rect(ix, iy, iw, ih, fill="surface", stroke="blue", sw=1.5)
    s.text(ix + 14, iy + 24, "Atlantis DB", 14.5, weight=650)
    # Query bar.
    s.rect(ix + 14, iy + 36, iw - 28, 30, fill="panel", stroke="axis", rx=6)
    s.text(ix + iw / 2, iy + 56, "One Cypher query", 13.5, weight=600, anchor="middle", mono=True)
    # Modes.
    modes = [("blue", "Vector", "HNSW"), ("orange", "Keyword", "BM25"), ("aqua", "Graph", "traversal")]
    mw = (iw - 28 - 16) / 3
    for i, (c, a, b) in enumerate(modes):
        mx = ix + 14 + i * (mw + 8)
        my = iy + 82
        s.line(mx + mw / 2, iy + 66, mx + mw / 2, my - 3, stroke="ink2", sw=1.5, arrow="ink2")
        s.rect(mx, my, mw, 46, fill=c, stroke=c, opacity=0.14, sw=1.5)
        s.text(mx + mw / 2, my + 20, a, 13.5, weight=600, anchor="middle")
        s.text(mx + mw / 2, my + 37, b, 12.5, "ink2", anchor="middle")
    s.rect(ix + 14, iy + 142, iw - 28, 30, fill="blue", stroke="blue", rx=6, opacity=0.14, sw=1.5)
    s.text(ix + iw / 2, iy + 162, "One MVCC snapshot · ACID", 13.5, weight=600, anchor="middle")
    for i, line in enumerate(
        ["1 query, 1 transaction, no network hop.",
         "BM25 and read-your-writes search: in progress (F4, F5)."]
    ):
        s.text(rx_ + 20, H - 70 + i * 20, line, 13, "ink2" if i == 0 else "muted")
    s.save("one-query", mode)


# --------------------------------------------------------------------------
# 2. F2 benchmark: before and after
# --------------------------------------------------------------------------

# Stages, oldest first: (legend label, colour key, report).
STAGES = [
    ("Before: 8a66a87 (HNSW fix)", "before", "2026-11-16-small-mem-hnsw-fix"),
    ("Vector core: SIMD, arena, locks", "blue_soft", "2026-11-16-small-mem-f2-core"),
    ("+ parallel build", "blue", "2026-11-16-small-mem-parallel-build"),
]

BENCH = [
    # (metric, direction note, unit formatter, value per stage, verdict)
    ("Index build", "lower is better", lambda v: f"{v:.1f} s", (185.8, 20.9, 4.96), "37× faster"),
    ("Vector top-10, p50", "lower is better", lambda v: f"{v:.2f} ms", (1.87, 0.27, 0.264), "7.1× faster"),
    ("Graph-RAG, 8 threads", "higher is better", lambda v: f"{v:,.0f} QPS", (225, 3041, 4168), "18.5× the throughput"),
]


def bench(mode: str) -> None:
    t = THEMES[mode]
    bar, step = 20, 26
    W, head = 880, 108
    row_h = 24 + step * len(STAGES)
    H = head + row_h * len(BENCH) + 40
    desc = "; ".join(
        f"{m}: " + ", ".join(f"{lbl} {f(v)}" for (lbl, _, _), v in zip(STAGES, vals)) + f" ({verdict})"
        for m, _, f, vals, verdict in BENCH
    )
    s = Svg(W, H, t, "F2 vector core: before and after", desc + ". Recall@10 stays 1.000.")
    s.text(28, 40, "F2 vector core: before and after", 18, weight=650)
    s.text(
        28, 64,
        "Retrieval Bench, 20,000 chunks × 768 dims, in memory, 12 CPUs. Recall@10 stays 1.000 throughout.",
        13.5, "ink2",
    )
    lgx = 28
    for label, key, _ in STAGES:
        s.rect(lgx, 79, 12, 12, fill=key, stroke=None, rx=3)
        s.text(lgx + 18, 89.5, label, 13, "ink2")
        lgx += 18 + len(label) * 6.9 + 26

    x0, maxw = 330, W - 330 - 120
    short = ["Before", "Core", "Parallel"]
    for i, (metric, note, fmt, vals, verdict) in enumerate(BENCH):
        y = head + i * row_h
        s.line(28, y, W - 28, y, stroke="grid")
        s.text(28, y + 32, metric, 15, weight=600)
        s.text(28, y + 52, note, 13, "muted")
        s.text(28, y + 74, verdict, 13.5, "ink", weight=650)
        top = max(vals)
        last = len(vals) - 1
        for j, ((_, key, _), v) in enumerate(zip(STAGES, vals)):
            by = y + 12 + j * step
            w = max(6.0, maxw * v / top)
            r = 4
            s.add(
                f'<path d="M{x0},{by} H{x0 + w - r} A{r},{r} 0 0 1 {x0 + w},{by + r} '
                f'V{by + bar - r} A{r},{r} 0 0 1 {x0 + w - r},{by + bar} H{x0} Z" fill="{t[key]}"/>'
            )
            s.text(x0 - 10, by + 15, short[j], 13, "muted", anchor="end")
            s.text(x0 + w + 8, by + 15, fmt(v), 13.5, "ink", weight=650 if j == last else 400)
        s.line(x0, y + 6, x0, y + row_h - 6, stroke="axis")
    s.line(28, head + row_h * len(BENCH), W - 28, head + row_h * len(BENCH), stroke="grid")
    s.text(
        28, H - 16,
        "Each row has its own scale. Reports: benches/results/2026-11-16-small-mem-{hnsw-fix,f2-core,parallel-build}.md",
        12.5, "muted",
    )
    s.save("bench-f2", mode)


# --------------------------------------------------------------------------
# 3. Roadmap focus track
# --------------------------------------------------------------------------

ROADMAP = [
    ("F1", "Retrieval", "Bench", "partial"),
    ("F2", "Vector", "core", "partial"),
    ("F3", "Filtered", "search", "done"),
    ("F4", "Consistent", "search", "next"),
    ("F5", "BM25 +", "fusion", "designed"),
    ("F6", "Group", "commit", "done"),
    ("F7", "Python", "package", "designed"),
    ("F8", "Publish", "results", "designed"),
]

STATE_LABEL = {
    "done": "Done",
    "partial": "Partly done",
    "next": "Next",
    "designed": "Designed",
}


def glyph(s: Svg, cx: float, cy: float, state: str) -> None:
    t = s.t
    r = 13
    if state == "done":
        s.add(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{t["blue"]}"/>')
        s.add(
            f'<path d="M{cx - 5.5},{cy + 0.5} L{cx - 1.5},{cy + 4.5} L{cx + 6},{cy - 4}" fill="none" '
            f'stroke="{t["surface"]}" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"/>'
        )
    elif state == "partial":
        s.add(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{t["surface"]}" stroke="{t["blue"]}" stroke-width="2.5"/>')
        s.add(
            f'<path d="M{cx},{cy - r + 4} A{r - 4},{r - 4} 0 0 1 {cx},{cy + r - 4} Z" fill="{t["blue"]}"/>'
        )
    elif state == "next":
        s.add(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{t["surface"]}" stroke="{t["blue"]}" stroke-width="2.5"/>')
        s.add(f'<circle cx="{cx}" cy="{cy}" r="3.5" fill="{t["blue"]}"/>')
    else:
        s.add(
            f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{t["surface"]}" stroke="{t["muted"]}" '
            f'stroke-width="2" stroke-dasharray="4 3"/>'
        )


def roadmap(mode: str) -> None:
    t = THEMES[mode]
    W, H = 880, 236
    desc = "; ".join(f"{k} {a} {b}: {STATE_LABEL[st]}" for k, a, b, st in ROADMAP)
    s = Svg(W, H, t, "Focus track: the transactional hybrid retrieval path", desc)
    s.text(28, 40, "Focus track", 18, weight=650)
    s.text(28, 63, "The transactional hybrid retrieval path, in order", 13.5, "ink2")
    left, right, cy = 70, W - 70, 110
    step = (right - left) / (len(ROADMAP) - 1)
    for i in range(len(ROADMAP) - 1):
        x1, x2 = left + i * step + 15, left + (i + 1) * step - 15
        done = ROADMAP[i][3] != "designed" and ROADMAP[i + 1][3] != "designed"
        s.line(x1, cy, x2, cy, stroke="blue" if done else "axis", sw=2)
    for i, (k, a, b, st) in enumerate(ROADMAP):
        cx = left + i * step
        glyph(s, cx, cy, st)
        s.text(cx, cy + 36, k, 14, weight=650, anchor="middle")
        s.text(cx, cy + 55, a, 13, "ink2", anchor="middle")
        s.text(cx, cy + 72, b, 13, "ink2", anchor="middle")
    # Legend.
    lx, ly = 28, H - 22
    for st in ("done", "partial", "next", "designed"):
        glyph(s, lx + 10, ly - 5, st)
        s.text(lx + 30, ly, STATE_LABEL[st], 13, "ink2")
        lx += 30 + len(STATE_LABEL[st]) * 7.2 + 28
    s.save("roadmap", mode)


# --------------------------------------------------------------------------
# 4. Architecture stack
# --------------------------------------------------------------------------

LAYERS = [
    # (crate or label, description, planned?)
    [("atlantis-engine", "transactions, Cypher execution, derived indexes", False)],
    [("atlantis-cypher", "hand-written lexer and parser · planner (planned)", False)],
    [
        ("atlantis-vector", "HNSW, SIMD kernels, vector arena", False),
        ("atlantis-text", "BM25 inverted index", True),
    ],
    [("atlantis-storage", "records, edges, adjacency, table catalog", False)],
    [("atlantis-kv", "ordered transactional KV, MVCC, write-ahead log, checkpoints", False)],
    [("atlantis-core", "values, record ids, errors, config", False)],
]

FRONTS = [("Rust, embedded", False), ("Python, embedded", True), ("Server and SDKs", True)]


def architecture(mode: str) -> None:
    t = THEMES[mode]
    W, rh, gap = 880, 50, 10
    top = 140
    H = top + len(LAYERS) * (rh + gap) + 46
    desc = "Layers from top: " + "; ".join(
        ", ".join(f"{n} ({d}{', planned' if p else ''})" for n, d, p in row) for row in LAYERS
    )
    s = Svg(W, H, t, "Atlantis DB architecture", desc)
    s.text(28, 40, "Architecture at a glance", 18, weight=650)
    # Front ends.
    fw = (W - 56 - 2 * 12) / 3
    for i, (n, planned) in enumerate(FRONTS):
        x = 28 + i * (fw + 12)
        s.rect(x, 60, fw, 40, fill="surface" if planned else "panel", stroke="muted" if planned else "axis", dash=planned)
        s.text(x + fw / 2, 85, n, 14, "muted" if planned else "ink", weight=600, anchor="middle")
        s.line(x + fw / 2, 102, x + fw / 2, top - 6, stroke="muted" if planned else "ink2", sw=1.5,
               dash=planned, arrow="muted" if planned else "ink2")
    for i, row in enumerate(LAYERS):
        y = top + i * (rh + gap)
        cw = (W - 56 - (len(row) - 1) * 12) / len(row)
        for j, (name, d, planned) in enumerate(row):
            x = 28 + j * (cw + 12)
            s.rect(x, y, cw, rh, fill="surface" if planned else "panel",
                   stroke="muted" if planned else "axis", dash=planned)
            if not planned:
                s.add(f'<rect x="{x}" y="{y + 10}" width="4" height="{rh - 20}" rx="2" fill="{t["blue"]}"/>')
            s.text(x + 18, y + 31, name, 14.5, "muted" if planned else "ink", weight=650, mono=True)
            dx = x + (206 if len(row) == 1 else 18 + len(name) * 9.1 + 18)
            s.text(dx, y + 31, d + (" (planned)" if planned and "planned" not in d else ""), 13.5,
                   "muted" if planned else "ink2")
    # Legend.
    ly = H - 22
    s.rect(28, ly - 13, 26, 16, fill="panel", stroke="axis", rx=4)
    s.add(f'<rect x="28" y="{ly - 10}" width="3" height="10" rx="1.5" fill="{t["blue"]}"/>')
    s.text(62, ly, "Implemented", 13, "ink2")
    s.rect(170, ly - 13, 26, 16, fill="surface", stroke="muted", rx=4, dash=True)
    s.text(204, ly, "Planned", 13, "ink2")
    s.save("architecture", mode)


if __name__ == "__main__":
    for m in THEMES:
        one_query(m)
        bench(m)
        roadmap(m)
        architecture(m)
    print("wrote", sorted(p.name for p in OUT.glob("*.svg")))
