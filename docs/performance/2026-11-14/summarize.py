"""Collect the latest complete trials and regenerate the README's two SVGs.

python docs/performance/2026-11-14/summarize.py target/tabby-benchmark
Requires three idle launches per app/session count and five startup/throughput runs.
"""
from html import escape
import json
from pathlib import Path
import statistics
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def summarize(source):
    datasets = []
    for path in sorted(source.glob("*/results.json"), key=lambda p: p.stat().st_mtime):
        data = json.loads(path.read_text())
        if data["errors"]:
            continue
        if data["mode"] != "startup" and any(c["terminal"][:2] != [107, 40] for c in data["connections"]):
            continue
        if data["mode"] == "throughput" and (len(data["runs"]) != 5 or any(r["bytes"] != 100778552 for r in data["runs"])):
            continue
        if data["mode"] == "idle" and (len(data["runs"]) != 5 or len(data["connections"]) != data["sessions"]):
            continue
        if data["mode"] == "startup" and len(data["runs"]) != 5:
            continue
        datasets.append({"id": path.parent.name, **data})
    selected, summary = [], {}
    for app in ("komando", "tabby"):
        summary[app] = {}
        for mode in ("startup", "throughput", "idle"):
            for sessions in ((1, 10) if mode == "idle" else (1,)):
                count = 3 if mode == "idle" else 1
                trials = [d for d in datasets if d["app"] == app and d["mode"] == mode and d["sessions"] == sessions][-count:]
                assert len(trials) == count, (app, mode, sessions, "missing trials")
                selected.extend(trials)
                field = {"startup": "milliseconds", "throughput": "seconds", "idle": "private_working_set_bytes"}[mode]
                values = [statistics.median(r[field] for r in d["runs"]) for d in trials]
                samples = [r[field] for d in trials for r in d["runs"]]
                if mode == "startup":
                    values = samples
                metric = f"idle_{sessions}" if mode == "idle" else mode
                summary[app][metric] = {"median": statistics.median(values),
                    "range": [min(samples), max(samples)], "launch_medians": values,
                    "launch_count": 5 if mode == "startup" else count,
                    "samples_per_launch": 1 if mode == "startup" else 5}
                if mode == "idle":
                    counts = {r["process_count"] for d in trials for r in d["runs"]}
                    assert len(counts) == 1, counts
                    summary[app][metric]["process_count"] = counts.pop()
    return {"date": "2026-11-14", "memory_unit": "bytes; MB in the README means 1,000,000 bytes",
            "summary": summary, "trials": selected}


def chart(summary, dark):
    fg, muted = ("#f0f3f6", "#a4afbc") if dark else ("#1f2328", "#59636e")
    primary, secondary = ("#fb923c", "#8da7c5") if dark else ("#c4510d", "#607b9c")
    metrics = [("Window on screen after launch", "startup", 1, "ms", 0, "faster"),
               ("Memory with 1 SSH Session", "idle_1", 1e6, "MB", 1, "less memory"),
               ("Memory with 10 SSH Sessions", "idle_10", 1e6, "MB", 1, "less memory"),
               ("1,000,000 coloured log lines", "throughput", 1, "s", 2, "faster")]
    alt = "; ".join(f"{label}: Komando {summary['komando'][key]['median'] / divisor:.{digits}f} {unit}, "
                    f"Tabby {summary['tabby'][key]['median'] / divisor:.{digits}f} {unit}"
                    for label, key, divisor, unit, digits, _ in metrics)
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 828 426" width="828" height="426" '
           f'font-family="-apple-system, BlinkMacSystemFont, Segoe UI, Helvetica, Arial, sans-serif" role="img" aria-labelledby="title desc">',
           '<title id="title">Komando 0.4.0 compared with Tabby 1.0.238</title>',
           f'<desc id="desc">{escape(alt)}. Lower is better. Each metric has its own zero-based scale.</desc>',
           f'<text x="0" y="19" fill="{fg}" font-size="14" font-weight="600">Komando 0.4.0 · Tabby 1.0.238</text>',
           f'<text x="820" y="19" text-anchor="end" fill="{muted}" font-size="12">Lower is better</text>']
    for index, (label, key, divisor, unit, digits, conclusion) in enumerate(metrics):
        y = 52 + index * 94
        a, b = (summary[app][key]["median"] / divisor for app in ("komando", "tabby"))
        out += [f'<text x="0" y="{y}" fill="{fg}" font-size="13" font-weight="600">{escape(label)}</text>',
                f'<text x="820" y="{y+36}" text-anchor="end" fill="{fg}" font-size="20" font-weight="700">{b/a:.1f}×</text>',
                f'<text x="820" y="{y+54}" text-anchor="end" fill="{muted}" font-size="12">{conclusion}</text>']
        for row, (app, value, color) in enumerate((("Komando", a, primary), ("Tabby", b, secondary))):
            by, width = y + 10 + row * 27, 480 * value / max(a, b)
            text = f"{value:.{digits}f} {unit}"
            out += [f'<text x="0" y="{by+15}" fill="{fg if row == 0 else muted}" font-size="12">{app}</text>',
                    f'<rect x="78" y="{by}" width="{width:.2f}" height="20" rx="4" fill="{color}"><title>{app}: {text}</title></rect>',
                    f'<text x="{86+width:.2f}" y="{by+15}" fill="{fg if row == 0 else muted}" font-size="13" font-weight="{700 if row == 0 else 400}">{text}</text>']
    out += [f'<text x="0" y="418" fill="{muted}" font-size="11">Windows 11 · 2026-11-14 · matching 107 × 40 terminal grids · method and raw results linked below</text>', '</svg>']
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    report = summarize(Path(sys.argv[1]))
    (HERE / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    for dark in (False, True):
        (ROOT / "assets" / f"benchmark-tabby-{'dark' if dark else 'light'}.svg").write_text(chart(report["summary"], dark))
    print(json.dumps(report["summary"], indent=2))
