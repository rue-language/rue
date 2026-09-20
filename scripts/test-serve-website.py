#!/usr/bin/env python3
"""Preview behavior: invalidation, last-good output, reload and recovery."""
from __future__ import annotations

import functools
import importlib.util
import json
import tempfile
import threading
import time
import unittest
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.request import urlopen

spec = importlib.util.spec_from_file_location("serve_website", Path(__file__).with_name("serve-website.py"))
preview_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(preview_module)


class PreviewTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="gazette-preview-test-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)
        self.write("website/content/page.md", "First preview")
        self.write("website/spec-route-root.txt", "spec\n")
        self.write("docs/spec/src/_index.md", "Spec")
        self.write("website/build.sh", '''#!/usr/bin/env python3
import html
import sys
from pathlib import Path
root = Path(__file__).resolve().parent.parent
with (root / "calls").open("a") as log:
    log.write(sys.argv[1] + "\\n")
source = (root / "website/content/page.md").read_text()
if source.startswith("BAD"):
    print("content/page.md:1: invalid content")
    raise SystemExit(1)
output = Path(sys.argv[3])
output.mkdir(parents=True, exist_ok=True)
(output / "index.html").write_text("<body><p>" + html.escape(source) + "</p></body>")
if source == "First preview":
    (output / "old.html").write_text("Removed in the next build")
''')
        (self.root / "website/build.sh").chmod(0o755)
        self.preview = preview_module.Preview(self.root, "http://127.0.0.1:1111")

    def write(self, path, value):
        target = self.root / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(value)

    def wait_for(self, predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(0.02)
        self.fail("preview did not reach expected state: " + repr(self.preview.status()))

    def test_failed_build_keeps_last_good_output_and_recovers(self):
        self.assertTrue(self.preview.rebuild(True))
        before = (self.preview.output / "index.html").read_bytes()
        self.write("website/content/page.md", "BAD <script>not executable</script>")
        self.assertFalse(self.preview.rebuild(False))
        self.assertEqual(self.preview.revision, 1)
        self.assertEqual((self.preview.output / "index.html").read_bytes(), before)
        self.assertIn("content/page.md:1", self.preview.error)
        self.write("website/content/page.md", "Fixed preview")
        self.assertTrue(self.preview.rebuild(False))
        self.assertEqual(self.preview.revision, 2)
        self.assertEqual(self.preview.error, "")
        self.assertFalse((self.preview.output / "old.html").exists())
        self.assertEqual(list((self.root / "website").glob(".preview-stage-*")), [])
        self.assertEqual(list((self.root / "website").glob(".preview-old-*")), [])

    def test_watch_reuses_preparation_until_its_inputs_change(self):
        worker = threading.Thread(target=self.preview.watch, daemon=True)
        worker.start()
        try:
            self.wait_for(lambda: self.preview.revision == 1)
            self.write("website/content/page.md", "An ordinary prose edit")
            self.wait_for(lambda: self.preview.revision == 2)
            self.write("docs/spec/src/_index.md", "Changed preparation source")
            self.wait_for(lambda: self.preview.revision == 3)
            self.write("website/static/new.txt", "New asset")
            self.wait_for(lambda: self.preview.revision == 4)
            (self.root / "website/static/new.txt").unlink()
            self.wait_for(lambda: self.preview.revision == 5)
            self.assertEqual((self.root / "calls").read_text().splitlines(), [
                "--preview-build", "--preview-render", "--preview-build",
                "--preview-render", "--preview-render",
            ])
        finally:
            self.preview.stop.set()
            worker.join(timeout=5)
        self.assertFalse(worker.is_alive())

    def test_generated_outputs_do_not_trigger_rebuild_loops(self):
        before = preview_module.input_snapshot(self.root)
        for path in ["website/content/spec/copied.md", "website/content/errors/E0001/index.md",
                     "website/static/style.css", "website/static/status.json",
                     "website/static/performance-data.json", "website/source-excerpts.json",
                     "website/.preview/index.html"]:
            self.write(path, "generated")
        self.assertEqual(preview_module.input_snapshot(self.root), before)

    def test_http_injects_reload_only_into_preview_responses(self):
        self.assertTrue(self.preview.rebuild(True))
        handler = functools.partial(preview_module.Handler, preview=self.preview)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        worker = threading.Thread(target=server.serve_forever, daemon=True)
        worker.start()
        base = "http://127.0.0.1:%d" % server.server_port
        try:
            with urlopen(base + "/") as response:
                body = response.read().decode()
                self.assertEqual(response.headers["Cache-Control"], "no-store")
            self.assertIn('src="/_gazette/client.js" data-revision="1"', body)
            self.assertNotIn("/_gazette/", (self.preview.output / "index.html").read_text())
            with urlopen(base + "/_gazette/status") as response:
                self.assertEqual(json.load(response), dict(revision=1, error="", building=False))
            with urlopen(base + "/_gazette/client.js") as response:
                self.assertIn(b"location.reload()", response.read())
        finally:
            server.shutdown()
            server.server_close()
            worker.join(timeout=5)


if __name__ == "__main__":
    unittest.main()
