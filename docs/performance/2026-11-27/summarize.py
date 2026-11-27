"""Collect the latest complete trials and regenerate the README's two benchmark SVGs.

python docs/performance/2026-11-27/summarize.py target/benchmark-2026-11-27
python docs/performance/2026-11-27/summarize.py --charts-only
Requires ten startup launches, ten throughput runs, and three idle launches of five samples
for each application and Session count.
"""
from html import escape
import json
import math
from pathlib import Path
import statistics
import sys

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
DATE = "2026-11-27"
GRID = [107, 40]
FIXTURE_BYTES = 100_778_552
APPS = ("komando", "wt", "tabby")
RUNS = {"startup": 10, "throughput": 10, "idle": 5}
IDLE_LAUNCHES = 3
FIELD = {"startup": "milliseconds", "throughput": "seconds", "idle": "private_working_set_bytes"}


def complete(data):
    if data["errors"] or len(data["runs"]) != RUNS[data["mode"]]:
        return False
    if data["mode"] != "startup" and any(c["terminal"][:2] != GRID for c in data["connections"]):
        return False
    if data["mode"] == "throughput":
        return all(r["bytes"] == FIXTURE_BYTES for r in data["runs"])
    if data["mode"] == "idle":
        return len(data["connections"]) == data["sessions"]
    return True


def summarize(source):
    datasets = []
    for path in sorted(source.glob("*/results.json"), key=lambda p: p.stat().st_mtime):
        data = json.loads(path.read_text())
        if complete(data):
            datasets.append({"id": path.parent.name, **data})
    selected, summary = [], {}
    for app in APPS:
        summary[app] = {}
        for mode in ("startup", "throughput", "idle"):
            for sessions in ((1, 10) if mode == "idle" else (1,)):
                count = IDLE_LAUNCHES if mode == "idle" else 1
                trials = [d for d in datasets if d["app"] == app and d["mode"] == mode
                          and d["sessions"] == sessions][-count:]
                assert len(trials) == count, (app, mode, sessions, "missing trials")
                selected.extend(trials)
                samples = [r[FIELD[mode]] for d in trials for r in d["runs"]]
                # Memory: median of each launch's median. Startup and throughput: median of all runs.
                values = [statistics.median(r[FIELD[mode]] for r in d["runs"]) for d in trials] \
                    if mode == "idle" else samples
                metric = f"idle_{sessions}" if mode == "idle" else mode
                summary[app][metric] = {"median": statistics.median(values), "min": min(samples),
                                        "max": max(samples), "launches": count,
                                        "samples_per_launch": RUNS[mode] if mode == "idle" else 1,
                                        "runs": len(samples)}
                if mode == "idle":
                    counts = {r["process_count"] for d in trials for r in d["runs"]}
                    assert len(counts) == 1, counts
                    summary[app][metric]["process_count"] = counts.pop()
                if mode == "throughput":
                    summary[app][metric]["megabytes_per_second"] = FIXTURE_BYTES / 1e6 / statistics.median(values)
    return {"date": DATE, "memory_unit": "bytes; MB in the README means 1,000,000 bytes",
            "summary": summary, "trials": selected}


NAMES = {"komando": "Komando", "wt": "Windows Terminal", "tabby": "Tabby"}
# label, summary key, divisor, unit, decimals
METRICS = [("Startup to visible window", "startup", 1, "ms", 0),
           ("Memory, 1 idle SSH Session", "idle_1", 1e6, "MB", 1),
           ("Memory, 10 idle SSH Sessions", "idle_10", 1e6, "MB", 1),
           ("Print 1,000,000 coloured log lines", "throughput", 1, "s", 2)]


def nice_ticks(top):
    """Zero-based ticks at 1/2/2.5/5 x 10^n, three to five of them, covering top."""
    raw = top / 4
    power = 10 ** math.floor(math.log10(raw))
    step = next(m * power for m in (1, 2, 2.5, 5, 10) if m * power >= raw)
    return [i * step for i in range(math.ceil(top / step - 1e-9) + 1)]


def tick_text(value):
    return f"{value:,.0f}" if value >= 10 or value == int(value) else f"{value:g}"


def chart(summary, environment, dark):
    c = {"surface": "#0d1117", "text": "#f0f3f6", "muted": "#9198a1", "grid": "#262c36",
         "axis": "#3d444d", "whisker": "#f0f3f6",
         "series": {"komando": "#3987e5", "wt": "#d95926", "tabby": "#199e70"}} if dark else \
        {"surface": "#ffffff", "text": "#1f2328", "muted": "#59636e", "grid": "#eaeef2",
         "axis": "#c8d1da", "whisker": "#1f2328",
         "series": {"komando": "#2a78d6", "wt": "#eb6834", "tabby": "#1baf7a"}}
    versions = environment["versions"]
    width, pad, gap = 860, 24, 28
    panel_w = (width - 2 * pad - gap) / 2
    name_w, value_w, band, bar = 118, 66, 30, 18
    panel_h = 26 + 3 * band + 26
    top = 118
    height = top + 2 * panel_h + gap + 76

    def value_text(key, value, unit, digits):
        # At most three significant figures: 30.0 MB, 321 MB.
        digits = min(digits, max(0, 2 - math.floor(math.log10(value))))
        return f"{value:,.{digits}f} {unit}"

    alt = "; ".join(f"{label}: " + ", ".join(
        f"{NAMES[app]} {value_text(key, summary[app][key]['median'] / div, unit, digits)}" for app in APPS)
        for label, key, div, unit, digits in METRICS)
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" width="{width}" '
           f'height="{height}" font-family="-apple-system, BlinkMacSystemFont, Segoe UI, Helvetica, Arial, '
           f'sans-serif" role="img" aria-labelledby="title desc">',
           '<title id="title">Komando, Windows Terminal and Tabby benchmarks</title>',
           f'<desc id="desc">{escape(alt)}. Lower is better. Bars are medians, whiskers the fastest '
           f'and slowest run. Each panel has its own zero-based scale.</desc>',
           # An explicit surface keeps the text readable where GitHub's theme selectors are ignored.
           f'<rect width="{width}" height="{height}" rx="12" fill="{c["surface"]}"/>',
           f'<text x="{pad}" y="38" fill="{c["text"]}" font-size="17" font-weight="600">'
           f'SSH terminals on Windows 11, lower is better</text>',
           f'<text x="{pad}" y="60" fill="{c["muted"]}" font-size="13">Bars show the median; whiskers '
           f'span the fastest and slowest run. Each panel has its own scale, starting at zero.</text>']
    x = pad
    for app in APPS:
        label = f"{NAMES[app]} {versions[app]}" + (" + OpenSSH 9.5" if app == "wt" else "")
        out += [f'<rect x="{x}" y="80" width="12" height="12" rx="3" fill="{c["series"][app]}"/>',
                f'<text x="{x + 18}" y="90.5" fill="{c["text"]}" font-size="13">{escape(label)}</text>']
        x += 18 + 7.1 * len(label) + 26
    for index, (label, key, div, unit, digits) in enumerate(METRICS):
        px = pad + (index % 2) * (panel_w + gap)
        py = top + (index // 2) * (panel_h + gap)
        x0, x1 = px + name_w, px + panel_w - value_w
        ticks = nice_ticks(max(summary[app][key]["max"] / div for app in APPS))
        scale = (x1 - x0) / ticks[-1]
        plot_top, plot_bottom = py + 24, py + 24 + 3 * band
        out.append(f'<text x="{px}" y="{py + 10}" fill="{c["text"]}" font-size="14" font-weight="600">'
                   f'{escape(label)} <tspan fill="{c["muted"]}" font-weight="400">({unit})</tspan></text>')
        for tick in ticks:
            tx = x0 + tick * scale
            out += [f'<line x1="{tx:.1f}" y1="{plot_top}" x2="{tx:.1f}" y2="{plot_bottom}" '
                    f'stroke="{c["axis"] if tick == 0 else c["grid"]}" stroke-width="1"/>',
                    f'<text x="{tx:.1f}" y="{plot_bottom + 16}" text-anchor="middle" fill="{c["muted"]}" '
                    f'font-size="11" style="font-variant-numeric: tabular-nums">{tick_text(tick)}</text>']
        for row, app in enumerate(APPS):
            m = summary[app][key]
            median, low, high = m["median"] / div, m["min"] / div, m["max"] / div
            cy = plot_top + row * band + band / 2
            end = x0 + median * scale
            r = min(4, end - x0)
            tip = (f'{NAMES[app]} {versions[app]}: median {value_text(key, median, unit, digits)}, '
                   f'range {value_text(key, low, unit, digits)} to {value_text(key, high, unit, digits)}, '
                   f'{m["runs"]} samples')
            out += [f'<text x="{px}" y="{cy + 4.5}" fill="{c["muted"]}" font-size="12.5">{NAMES[app]}</text>',
                    f'<path d="M{x0:.2f},{cy - bar / 2:.2f} H{end - r:.2f} A{r:.2f},{r:.2f} 0 0 1 {end:.2f},'
                    f'{cy - bar / 2 + r:.2f} V{cy + bar / 2 - r:.2f} A{r:.2f},{r:.2f} 0 0 1 {end - r:.2f},'
                    f'{cy + bar / 2:.2f} H{x0:.2f} Z" fill="{c["series"][app]}"><title>{escape(tip)}</title></path>']
            lx, hx = x0 + low * scale, x0 + high * scale
            out += [f'<g stroke="{c["whisker"]}" stroke-width="1.5" stroke-linecap="round" opacity="0.85">'
                    f'<line x1="{lx:.2f}" y1="{cy}" x2="{hx:.2f}" y2="{cy}"/>'
                    f'<line x1="{lx:.2f}" y1="{cy - 4}" x2="{lx:.2f}" y2="{cy + 4}"/>'
                    f'<line x1="{hx:.2f}" y1="{cy - 4}" x2="{hx:.2f}" y2="{cy + 4}"/></g>',
                    f'<text x="{max(end, hx) + 7:.2f}" y="{cy + 4.5}" fill="{c["text"]}" font-size="12.5" '
                    f'font-weight="600" style="font-variant-numeric: tabular-nums">'
                    f'{value_text(key, median, unit, digits)}</text>']
    lines = [f"{environment['machine_short']}, measured {DATE}. Loopback SSH server, "
             f"107 × 40 grid, 100,000-line Scrollback.",
             "Startup: 10 warm launches. Memory: private working set of every process, 3 launches × 5 samples. "
             "Log lines: 10 runs of 100.8 MB.",
             "Komando and Tabby use their built-in SSH clients; Windows Terminal runs ssh.exe through ConPTY."]
    out += [f'<text x="{pad}" y="{height - 56 + i * 17}" fill="{c["muted"]}" font-size="12">{escape(line)}</text>'
            for i, line in enumerate(lines)] + ['</svg>']
    return "\n".join(out) + "\n"


if __name__ == "__main__":
    if sys.argv[1] == "--charts-only":
        report = json.loads((HERE / "results.json").read_text())
    else:
        report = summarize(Path(sys.argv[1]))
        (HERE / "results.json").write_text(json.dumps(report, indent=2) + "\n")
    environment = json.loads((HERE / "environment.json").read_text())
    for dark in (False, True):
        name = f"benchmark-{'dark' if dark else 'light'}.svg"
        (ROOT / "assets" / name).write_text(chart(report["summary"], environment, dark), encoding="utf-8")
    print(json.dumps(report["summary"], indent=2))
