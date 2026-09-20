#!/usr/bin/env python3
"""Watch the website's inputs and serve successful Gazette builds locally.

The build script owns preparation and rendering for both CI and this host.
Only its prepared inputs are reused: Gazette and Tailwind still run for every
edit, so page removal, template changes and new CSS classes cannot go stale.
"""
from __future__ import annotations

import argparse
import functools
import io
import json
import os
import signal
import subprocess
import tempfile
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from shutil import rmtree
from urllib.parse import urlsplit


CLIENT = """(() => {
  let revision = Number(document.currentScript.dataset.revision);
  async function poll() {
    try {
      const state = await (await fetch('/_gazette/status', {cache: 'no-store'})).json();
      if (state.revision !== revision) { location.reload(); return; }
      let panel = document.getElementById('gazette-preview-error');
      if (state.error) {
        if (!panel) {
          panel = document.createElement('pre');
          panel.id = 'gazette-preview-error';
          panel.setAttribute('role', 'alert');
          panel.style.cssText = 'position:fixed;inset:auto 1rem 1rem;max-height:45vh;overflow:auto;z-index:9999;padding:1rem;background:#281d1d;color:#fff;border:2px solid #c76b62;white-space:pre-wrap;font:14px/1.5 monospace';
          document.body.appendChild(panel);
        }
        panel.textContent = 'Build failed — showing the last successful preview.\\n\\n' + state.error;
      } else if (panel) panel.remove();
    } catch (_) { /* A stopped/restarting local server will answer later. */ }
    setTimeout(poll, 750);
  }
  poll();
})();
"""


def input_snapshot(root: Path) -> tuple[dict, dict]:
    """Separate repository preparation inputs from ordinary website edits."""
    website = root / "website"
    route_file = website / "spec-route-root.txt"
    route = route_file.read_text().strip() if route_file.exists() else "spec"
    generated = {
        "website/static/style.css", "website/static/status.json",
        "website/static/performance-data.json", "website/source-excerpts.json",
    }
    ignored_content = ["website/content/errors/", "website/content/" + route + "/"]

    def collect(names):
        result = {}
        for name in names:
            path = root / name
            paths = path.rglob("*") if path.is_dir() else [path]
            for candidate in paths:
                rel = candidate.relative_to(root).as_posix()
                if rel in generated or any(rel.startswith(prefix) for prefix in ignored_content):
                    continue
                if any(part.startswith((".errors.", ".public.", ".preview")) for part in candidate.parts):
                    continue
                try:
                    if candidate.is_file():
                        stat = candidate.stat()
                        result[rel] = (stat.st_mtime_ns, stat.st_size)
                except FileNotFoundError:
                    # An editor's atomic replacement is observed next poll.
                    pass
        return result

    render = collect([
        "website/content", "website/templates", "website/css", "website/static",
        "website/syntaxes", "website/config.toml",
    ])
    prepare = collect([
        "docs/spec/src", "crates", "std", "examples/gazette", "performance",
        "website/spec-route-root.txt", "website/build.sh", "scripts/serve-website.py",
        "scripts/extract-source-excerpts.py", "scripts/generate-site-status.py",
        "scripts/annotate-performance-commits.py", "BUCK", "examples/BUCK",
        "buck2", "tailwindcss", ".buckconfig", "rue_rules.bzl",
    ])
    # A local fetch can change the data branch without touching source files.
    refs = subprocess.run(
        ["git", "rev-parse", "HEAD", "origin/performance-data-v1"], cwd=root,
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, check=False,
    )
    prepare["git-revisions"] = refs.stdout
    return prepare, render


class Preview:
    def __init__(self, root: Path, base_url: str):
        self.root = root
        self.output = root / "website/.preview"
        self.base_url = base_url
        self.lock = threading.RLock()
        self.stop = threading.Event()
        self.revision = 0
        self.error = ""
        self.building = False
        self.process = None

    def status(self):
        with self.lock:
            return dict(revision=self.revision, error=self.error, building=self.building)

    def rebuild(self, prepare: bool) -> bool:
        stage = Path(tempfile.mkdtemp(prefix=".preview-stage-", dir=self.output.parent))
        try:
            with self.lock:
                self.building = True
            mode = "--preview-build" if prepare else "--preview-render"
            command = [str(self.root / "website/build.sh"), mode, self.base_url, str(stage)]
            with tempfile.TemporaryFile() as log:
                process = subprocess.Popen(
                    command, cwd=self.root, stdout=log, stderr=subprocess.STDOUT,
                    start_new_session=True,
                )
                self.process = process
                while process.poll() is None:
                    if self.stop.wait(0.1):
                        os.killpg(process.pid, signal.SIGTERM)
                        try:
                            process.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            os.killpg(process.pid, signal.SIGKILL)
                            process.wait()
                        return False
                log.seek(0)
                details = log.read().decode("utf-8", errors="replace")
            print(details, end="", flush=True)
            if process.returncode:
                with self.lock:
                    self.error = details[-16000:] or "The website build failed."
                return False
            with self.lock:
                backup = Path(tempfile.mkdtemp(prefix=".preview-old-", dir=self.output.parent))
                backup.rmdir()
                if self.output.exists():
                    self.output.rename(backup)
                try:
                    stage.rename(self.output)
                except OSError:
                    if backup.exists():
                        backup.rename(self.output)
                    raise
                finally:
                    if backup.exists():
                        rmtree(backup)
                self.error = ""
                self.revision += 1
            return True
        except OSError as error:
            with self.lock:
                self.error = str(error)
            print("Preview build failed: " + str(error), flush=True)
            return False
        finally:
            self.process = None
            with self.lock:
                self.building = False
            if stage.exists():
                rmtree(stage)

    def watch(self):
        before = input_snapshot(self.root)
        prepared = self.rebuild(prepare=True)
        # Keep the pre-build snapshot: edits during a build must not disappear.
        while not self.stop.wait(0.5):
            after = input_snapshot(self.root)
            if after == before:
                continue
            # Coalesce an editor's save/rename burst, then capture what we build.
            if self.stop.wait(0.15):
                break
            after = input_snapshot(self.root)
            needs_prepare = not prepared or after[0] != before[0]
            before = after
            succeeded = self.rebuild(prepare=needs_prepare)
            if needs_prepare:
                prepared = succeeded


class Handler(SimpleHTTPRequestHandler):
    def __init__(self, *args, preview: Preview, **kwargs):
        self.preview = preview
        super().__init__(*args, directory=str(preview.output), **kwargs)

    def log_message(self, message, *args):
        if urlsplit(self.path).path != "/_gazette/status":
            super().log_message(message, *args)

    def end_headers(self):
        self.send_header("Cache-Control", "no-store")
        super().end_headers()

    def response(self, body: bytes, content_type: str, status=200):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        return io.BytesIO(body)

    def send_head(self):
        path = urlsplit(self.path).path
        if path == "/_gazette/status":
            return self.response(json.dumps(self.preview.status()).encode(), "application/json")
        if path == "/_gazette/client.js":
            return self.response(CLIENT.encode(), "text/javascript; charset=utf-8")
        with self.preview.lock:
            target = Path(self.translate_path(self.path))
            if target.is_dir() and path.endswith("/"):
                target = target / "index.html"
            if target.suffix == ".html" and target.is_file():
                html = target.read_bytes()
                client = ('<script src="/_gazette/client.js" data-revision="%d"></script>'
                          % self.preview.revision).encode()
                html = html.replace(b"</body>", client + b"</body>") if b"</body>" in html else html + client
                return self.response(html, "text/html; charset=utf-8")
            if not self.preview.output.exists() and path == "/":
                return self.response(
                    b'<!doctype html><title>Gazette preview</title><p>Building the first preview...</p>'
                    b'<script src="/_gazette/client.js" data-revision="0"></script>',
                    "text/html; charset=utf-8", status=503,
                )
            return super().send_head()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--port", type=int, default=1111)
    args = parser.parse_args()
    if not 1 <= args.port <= 65535:
        parser.error("port must be between 1 and 65535")
    root = Path(__file__).resolve().parent.parent
    preview = Preview(root, "http://127.0.0.1:%d" % args.port)
    server = ThreadingHTTPServer(("127.0.0.1", args.port), functools.partial(Handler, preview=preview))
    worker = threading.Thread(target=preview.watch, daemon=True)
    worker.start()
    print("Watching website inputs at " + preview.base_url, flush=True)
    try:
        server.serve_forever(poll_interval=0.2)
    except KeyboardInterrupt:
        pass
    finally:
        preview.stop.set()
        worker.join(timeout=10)
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
