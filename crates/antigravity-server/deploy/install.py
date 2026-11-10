"""First installation only. Run from a root-owned, private staging directory."""
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import shutil
import socket
import subprocess
import time
import urllib.request


def run(*args):
    subprocess.run(args, check=True, stdout=subprocess.DEVNULL)


def write_private(path, data):
    with open(str(path), "x") as handle:
        os.chmod(str(path), 0o600)
        handle.write(data)


def main():
    if os.geteuid() != 0:
        raise RuntimeError("Root required")
    staging = Path(__file__).resolve().parent
    root = Path("/opt/antigravity-upstream")
    state = Path("/var/lib/antigravity-upstream")
    config = Path("/etc/antigravity-upstream")
    snippet = Path("/etc/nginx/snippets/antigravity-upstream.conf")
    units = ["antigravity-backend.service", "antigravity-relay.service"]
    for path in [root, state, config, snippet] + [Path("/etc/systemd/system") / unit for unit in units]:
        if path.exists() or path.is_symlink():
            raise RuntimeError("Refusing existing deployment: " + str(path))
    for port in [8317, 8318, 8319]:
        with socket.socket() as listener:
            listener.bind(("127.0.0.1", port))
    # Only modify the unique HTTPS server for this domain in the primary config.
    nginx = Path("/etc/nginx/nginx.conf")
    original = nginx.read_text()
    marker = re.compile(r"(?m)^(\s*)server_name\s+dev\.guxiaoquan\.com\s*;")
    matches = list(marker.finditer(original))
    if len(matches) != 1 or "/antigravity/" in original:
        raise RuntimeError("Nginx target is ambiguous or already configured")
    match = matches[0]
    server_start = original.rfind("server {", 0, match.start())
    if server_start < 0 or "ssl_certificate" not in original[match.end():]:
        raise RuntimeError("Expected existing HTTPS server")
    backup = root / "nginx.conf.before"
    try:
        pwd.getpwnam("antigravity")
    except KeyError:
        run("useradd", "--system", "--home-dir", "/nonexistent", "--shell", "/sbin/nologin", "antigravity")
    account = pwd.getpwnam("antigravity")
    for path in [root, state, state / "auth", config]:
        path.mkdir(mode=0o700)
    write_private(backup, original)
    root.chmod(0o755)
    config.chmod(0o750)
    for path in [state, state / "auth", config]:
        os.chown(str(path), account.pw_uid, account.pw_gid)
    for binary in ["cliproxyapi", "cockpit-antigravity-server"]:
        shutil.copyfile(str(staging / binary), str(root / binary))
        (root / binary).chmod(0o755)
    auth = json.loads((staging / "account.json").read_text())
    if auth.get("type") != "antigravity" or not auth.get("refresh_token"):
        raise RuntimeError("Expected refreshable Antigravity authorization")
    # Existing per-account proxy overrides must not bypass the server's local proxy.
    auth.pop("proxy_url", None)
    auth.pop("proxy-url", None)
    auth.pop("base_url", None)
    write_private(state / "auth/account.json", json.dumps(auth))
    write_private(state / "api-key", os.urandom(32).hex() + "\n")
    settings = {
        "backend": {"backend_path": str(root / "cliproxyapi"), "state_dir": str(state),
                    "port": 8317, "model": "gemini-4-argon", "auto_start": False},
        "backend_port": 8318, "proxy_url": "http://127.0.0.1:7890", "egress_port": 8319
    }
    write_private(config / "config.json", json.dumps(settings, indent=2) + "\n")
    for path in [state / "auth/account.json", state / "api-key", config / "config.json"]:
        os.chown(str(path), account.pw_uid, account.pw_gid)
    for unit in units:
        shutil.copyfile(str(staging / unit), "/etc/systemd/system/" + unit)
        os.chmod("/etc/systemd/system/" + unit, 0o644)
    shutil.copyfile(str(staging / "nginx.conf"), str(snippet))
    snippet.chmod(0o644)
    changed = original[:match.end()] + "\n" + match.group(1) + "include " + str(snippet) + ";" + original[match.end():]
    nginx_changed = False
    try:
        run("systemctl", "daemon-reload")
        run("systemctl", "start", units[0])
        run("systemctl", "start", units[1])
        key = (state / "api-key").read_text().strip()
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        for attempt in range(30):
            try:
                req = urllib.request.Request("http://127.0.0.1:8317/v1/models", headers={"Authorization": "Bearer " + key})
                with opener.open(req, timeout=3) as response:
                    catalog = json.load(response)
                if not catalog.get("data"):
                    raise RuntimeError("Empty model catalog")
                break
            except Exception:
                if attempt == 29:
                    raise RuntimeError("Relay readiness timed out")
                time.sleep(2)
        nginx.write_text(changed)
        nginx_changed = True
        run("nginx", "-t")
        run("systemctl", "reload", "nginx")
        run("systemctl", "enable", *units)
    except Exception:
        if nginx_changed:
            nginx.write_text(original)
            run("nginx", "-t")
            run("systemctl", "reload", "nginx")
        subprocess.run(["systemctl", "disable", "--now"] + units, stdout=subprocess.DEVNULL)
        raise
    receipt = {
        "base_url": "https://dev.guxiaoquan.com/antigravity/v1",
        "backend_version": "8.0.50", "installed_at": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "nginx_backup": str(backup), "units": units,
        "binaries": {name: hashlib.sha256((root / name).read_bytes()).hexdigest()
                     for name in ["cliproxyapi", "cockpit-antigravity-server"]}
    }
    write_private(config / "deployment.json", json.dumps(receipt, indent=2) + "\n")
    (staging / "account.json").unlink()
    print(json.dumps(receipt))


if __name__ == "__main__":
    main()
