"""Windows GUI benchmark; see ../../benchmarks.md. Uses disposable config only.

pip install asyncssh==2.24.1 psutil==7.2.2 PyYAML==6.0.3
Build: cargo build --release --locked --example benchmark_client
Run: python compare.py --app komando|tabby --mode startup|idle|throughput
"""
import argparse
import asyncio
import base64
import ctypes
from ctypes import wintypes
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import uuid

import asyncssh
import psutil
import yaml

ROOT = Path(__file__).resolve().parents[3]
OUT = ROOT / "target" / "tabby-benchmark"
OUT.mkdir(parents=True, exist_ok=True)
USER32 = ctypes.WinDLL("user32", use_last_error=True)
DWM = ctypes.WinDLL("dwmapi")
CALLBACK = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
USER32.EnumWindows.argtypes = [CALLBACK, wintypes.LPARAM]
USER32.IsWindowVisible.argtypes = [wintypes.HWND]
USER32.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
USER32.GetWindowRect.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.RECT)]
DWM.DwmGetWindowAttribute.argtypes = [wintypes.HWND, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD]


def windows(pid):
    found = []

    @CALLBACK
    def visit(hwnd, _):
        owner, rect, cloaked = wintypes.DWORD(), wintypes.RECT(), wintypes.DWORD()
        USER32.GetWindowThreadProcessId(hwnd, ctypes.byref(owner))
        if owner.value == pid and USER32.IsWindowVisible(hwnd):
            USER32.GetWindowRect(hwnd, ctypes.byref(rect))
            DWM.DwmGetWindowAttribute(hwnd, 14, ctypes.byref(cloaked), 4)
            if not cloaked.value and rect.right - rect.left > 100 and rect.bottom - rect.top > 100:
                found.append([rect.right - rect.left, rect.bottom - rect.top])
        return True

    USER32.EnumWindows(visit, 0)
    return found


def processes(child):
    root = psutil.Process(child.pid)
    return [root, *root.children(recursive=True)]


def stop(child):
    try:
        owned = processes(child)
    except psutil.NoSuchProcess:
        return
    for proc in reversed(owned):
        try:
            proc.kill()
        except psutil.NoSuchProcess:
            pass
    psutil.wait_procs(owned, timeout=5)


def tabby_config(directory, port, key, sessions, width=892, height=748):
    directory.mkdir(parents=True, exist_ok=True)
    public = key.export_public_key().decode().split()[1]
    digest = base64.b64encode(hashlib.sha256(base64.b64decode(public)).digest()).decode()
    config = {
        "version": 7,
        "enableWelcomeTab": False,
        "enableAnalytics": False,
        "enableAutomaticUpdates": False,
        "recoverTabs": False,
        "terminal": {"scrollbackLines": 100000, "fontSize": 14, "font": "Cascadia Mono"},
        "ssh": {"knownHosts": [{"host": "127.0.0.1", "port": port, "type": "ssh-ed25519", "digest": digest}]},
        "profiles": [{"id": f"ssh:bench-{i}", "name": f"Benchmark {i}", "type": "ssh", "options": {
            "host": "127.0.0.1", "port": port, "user": f"bench{i}", "auth": "publicKey",
            "privateKeys": [str(directory.parent / "client_ed25519")], "reuseSession": False,
        }} for i in range(sessions)],
    }
    (directory / "config.yaml").write_text(yaml.safe_dump(config), encoding="utf-8")
    (directory / "window.json").write_text(json.dumps({"windowBoundaries": {
        "x": 100, "y": 100, "width": width, "height": height}, "maximized": False}))


def launch(app, mode, directory, port, host_key, sessions):
    env = os.environ.copy()
    if app == "komando":
        command = [str(ROOT / "target/release/examples/benchmark_client.exe"), str(directory),
                   str(directory.parent / "client_ed25519"), str(directory.parent / "host_ed25519.pub"),
                   str(port), str(sessions)]
    else:
        tabby_config(directory, port, host_key, sessions)
        env["TABBY_CONFIG_DIRECTORY"] = str(directory)
        command = [r"C:\Program Files\Tabby\Tabby.exe", f"--user-data-dir={directory}"]
        if mode != "startup":
            command += ["profile", "Benchmark 0"]
    with open(directory.parent / f"{app}-{mode}-{directory.name}.log", "wb") as log:
        return subprocess.Popen(command, env=env, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)


class Server(asyncssh.SSHServer):
    def begin_auth(self, username):
        return False


def logline(i):
    m = i % 16
    color, level = ("31", "ERROR") if m == 0 else ("33", "WARN ") if m in (1, 2) else ("35", "DEBUG") if m == 3 else ("32", "INFO ")
    service = ["api", "worker", "auth", "billing", "scheduler"][i % 5]
    return (f"\x1b[2m2026-11-13 {(i // 3600000) % 24:02}:{(i // 60000) % 60:02}:{(i // 1000) % 60:02}.{i % 1000:03}\x1b[0m "
            f"\x1b[{color}m{level}\x1b[0m \x1b[36m{service:<9}\x1b[0m GET /api/items/{i % 100000} "
            f"{500 if m == 0 else 200} in {i % 997}ms\r\n")


async def main(args):
    directory = OUT / f"{args.app}-{args.mode}-{uuid.uuid4().hex[:8]}"
    directory.mkdir()
    host_key = asyncssh.generate_private_key("ssh-ed25519")
    client_key = asyncssh.generate_private_key("ssh-ed25519")
    (directory / "client_ed25519").write_bytes(client_key.export_private_key())
    (directory / "host_ed25519.pub").write_bytes(host_key.export_public_key())
    connected, measurements, errors, shells = [], [], [], []
    blocks = []
    if args.mode == "throughput":
        blocks = ["".join(logline(i) for i in range(start, min(start + 600, args.lines))).encode()
                  for start in range(0, args.lines, 600)]

    async def shell(proc):
        shells.append(proc)
        connected.append({"user": proc.get_extra_info("username"), "terminal": proc.get_terminal_size()})
        print(json.dumps({"connected": connected[-1]}), flush=True)
        try:
            if args.mode != "throughput":
                proc.stdout.write(b"\r\nLoopback benchmark ready.\r\n$ ")
                while True:
                    try:
                        if not await proc.stdin.read(4096):
                            break
                    except asyncssh.TerminalSizeChanged:
                        continue
                return
            for run in range(args.runs):
                await asyncio.sleep(3)
                connected[0]["terminal"] = proc.get_terminal_size()
                if proc.get_terminal_size()[:2] != (107, 40):
                    raise RuntimeError(f"expected 107 x 40 terminal, got {proc.get_terminal_size()}")
                start = time.perf_counter()
                for block in blocks:
                    proc.stdout.write(block)
                    await proc.stdout.drain()
                sent = time.perf_counter() - start
                proc.stdout.write(b"\x1b[6n")
                buf = b""
                async with asyncio.timeout(60):
                    while not buf.endswith(b"R"):
                        try:
                            data = await proc.stdin.read(64)
                        except asyncssh.TerminalSizeChanged:
                            continue
                        if not data:
                            raise RuntimeError("terminal disconnected before cursor reply")
                        buf += data
                elapsed = time.perf_counter() - start
                row = {"run": run + 1, "seconds": elapsed, "sent_seconds": sent,
                       "bytes": sum(map(len, blocks)), "cursor_reply": buf.decode("ascii", "replace")}
                measurements.append(row)
                print(json.dumps(row), flush=True)
            await proc.stdin.read()
        except (asyncssh.Error, asyncio.IncompleteReadError):
            pass
        except Exception as exc:
            errors.append(repr(exc))

    server = await asyncssh.create_server(Server, "127.0.0.1", 0, server_host_keys=[host_key],
                                         process_factory=shell, encoding=None, line_editor=False)
    port = server.get_port()
    result = {"app": args.app, "mode": args.mode, "sessions": args.sessions, "server": asyncssh.__version__,
              "runs": [], "connections": connected, "errors": errors}
    child = None
    try:
        if args.mode == "startup":
            config_dir = directory / "config"
            if args.app == "tabby":
                tabby_config(config_dir, port, host_key, 0, width=1280, height=800)
            else:
                config_dir.mkdir()
                (config_dir / "settings.toml").write_text("check_for_updates = false\n")
            # One discarded launch warms each app's own caches before the timed launches.
            for i in range(-1, args.runs):
                env = os.environ.copy()
                env["KOMANDO_HOME"] = str(config_dir)
                env["TABBY_CONFIG_DIRECTORY"] = str(config_dir)
                command = [r"C:\Program Files\Komando\komando.exe"] if args.app == "komando" else [
                    r"C:\Program Files\Tabby\Tabby.exe", f"--user-data-dir={config_dir}"]
                with open(directory / f"startup-{i}.log", "wb") as logfile:
                    start = time.perf_counter()
                    child = subprocess.Popen(command, env=env, stdout=logfile, stderr=logfile,
                                             creationflags=subprocess.CREATE_NO_WINDOW)
                    while time.perf_counter() - start < 30:
                        rects = windows(child.pid)
                        if rects:
                            row = {"milliseconds": (time.perf_counter() - start) * 1000, "windows": rects}
                            if i >= 0:
                                result["runs"].append(row)
                                print(json.dumps(row), flush=True)
                            break
                        if child.poll() is not None:
                            raise RuntimeError(f"app exited: {child.returncode}")
                        await asyncio.sleep(0.005)
                    else:
                        raise TimeoutError("no visible window")
                    await asyncio.sleep(3)
                    stop(child)
                    child = None
                    await asyncio.sleep(1)
        else:
            child = launch(args.app, args.mode, directory / "config", port, host_key, args.sessions)
            deadline = time.perf_counter() + 40
            while len(connected) < args.sessions:
                if time.perf_counter() > deadline:
                    raise TimeoutError(f"only {len(connected)}/{args.sessions} sessions connected")
                if child.poll() is not None:
                    raise RuntimeError(f"app exited: {child.returncode}")
                if args.app == "tabby" and len(connected) and len(connected) < args.sessions:
                    # CLI asks the existing isolated instance to open the next built-in SSH tab.
                    i = len(connected)
                    env = os.environ.copy()
                    env["TABBY_CONFIG_DIRECTORY"] = str(directory / "config")
                    request = subprocess.Popen([r"C:\Program Files\Tabby\Tabby.exe",
                        f"--user-data-dir={directory / 'config'}", "profile", f"Benchmark {i}"], env=env,
                        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, creationflags=subprocess.CREATE_NO_WINDOW)
                    while request.poll() is None:
                        await asyncio.sleep(0.1)
                    await asyncio.sleep(1)
                await asyncio.sleep(0.1)
            if args.mode == "idle":
                await asyncio.sleep(10)
                for i, proc in enumerate(shells):
                    connected[i]["terminal"] = proc.get_terminal_size()
                    if proc.get_terminal_size()[:2] != (107, 40):
                        raise RuntimeError(f"expected 107 x 40 terminal, got {proc.get_terminal_size()}")
                if errors or any(proc.stdin.at_eof() for proc in shells):
                    raise RuntimeError(f"idle shell closed: {errors}")
                for _ in range(5):
                    procs = processes(child)
                    sample = {"private_working_set_bytes": sum(p.memory_full_info().uss for p in procs),
                              "process_count": len(procs), "windows": windows(child.pid),
                              "processes": [p.name() for p in procs]}
                    result["runs"].append(sample)
                    print(json.dumps(sample), flush=True)
                    await asyncio.sleep(1)
            else:
                deadline = time.perf_counter() + 240
                while len(measurements) < args.runs and not errors:
                    if time.perf_counter() > deadline:
                        raise TimeoutError("throughput did not finish")
                    await asyncio.sleep(0.1)
                result["runs"] = measurements
                if errors:
                    raise RuntimeError(errors)
    finally:
        if child:
            stop(child)
        server.close()
        await server.wait_closed()
        (directory / "results.json").write_text(json.dumps(result, indent=2))
        print(f"Results: {directory / 'results.json'}", flush=True)


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", choices=["komando", "tabby"], required=True)
    parser.add_argument("--mode", choices=["startup", "idle", "throughput"], required=True)
    parser.add_argument("--sessions", type=int, default=1)
    parser.add_argument("--runs", type=int, default=5)
    parser.add_argument("--lines", type=int, default=1_000_000)
    asyncio.run(main(parser.parse_args()))
