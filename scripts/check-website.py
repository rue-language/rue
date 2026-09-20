#!/usr/bin/env python3
"""Check the deployed website's generated features after its Gazette build.

These are consumer checks over output artifacts, not a second renderer. The
Website job runs them over the exact directory uploaded to GitHub Pages.
"""
from __future__ import annotations

import argparse
import json
import re
import sys
import xml.etree.ElementTree as ET
from email.utils import parsedate_to_datetime
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import urlsplit


class Page(HTMLParser):
    def __init__(self, path: Path):
        super().__init__(convert_charrefs=True)
        self.text = []
        self.classes = set()
        self.ids = set()
        self.refresh = None
        self.feed(path.read_text(encoding="utf-8"))

    def handle_starttag(self, tag, attrs):
        fields = dict(attrs)
        self.classes.update(fields.get("class", "").split())
        if fields.get("id"):
            self.ids.add(fields["id"])
        if tag == "meta" and fields.get("http-equiv", "").lower() == "refresh":
            self.refresh = fields.get("content", "")

    def handle_data(self, data):
        self.text.append(data)


def check(public: Path, source: Path) -> list[str]:
    failures = []

    def require(condition, message):
        if not condition:
            failures.append(message)

    required = [
        "index.html", "runtime/index.html", "performance/index.html",
        "blog/index.html", "blog/page/1/index.html", "errors/index.html",
        "404.html", "rss.xml", "blog/rss.xml", "sitemap.xml", "robots.txt",
        "search_index.en.json", "style.css", "syntax-dark.css", "syntax-light.css",
        "status.json", "performance-data.json", ".nojekyll",
    ]
    for name in required:
        require((public / name).is_file(), "missing output: " + name)
    if failures:
        return failures

    homepage = Page(public / "index.html")
    runtime = Page(public / "runtime/index.html")
    home_text = " ".join("".join(homepage.text).split())
    runtime_text = " ".join("".join(runtime.text).split())
    require("g-keyword" in homepage.classes, "homepage code is not highlighted")
    for name in ("syntax-dark.css", "syntax-light.css"):
        require(".g-keyword" in (public / name).read_text(), name + " has no Gazette token styles")
    require("rt-source-ports" in runtime.ids, "runtime source excerpts were not rendered")
    for label in ("self::render_nav", "examples/gazette/templates/spec/base.html",
                  "performance/ports/zola/templates/spec/base.html",
                  "performance/ports/hugo/layouts/_partials/spec-nav.html"):
        require(label in runtime_text, "runtime source excerpt missing: " + label)

    status = json.loads((public / "status.json").read_text())
    if status.get("commit"):
        require(status["commit"] in home_text, "homepage did not load status.json")
    performance = status.get("performance")
    if performance:
        require(str(performance["index"]) in home_text, "homepage lost fractional performance data")

    search = json.loads((public / "search_index.en.json").read_text())
    docs = search.get("documents", [])
    require(bool(docs), "search has no documents")
    refs = [doc["ref"] for doc in docs]
    require(len(refs) == len(set(refs)), "search contains duplicate document URLs")
    for doc in docs:
        require(all(key in doc for key in ("title", "description", "path", "body")),
                "incomplete search document: " + doc["ref"])
        route = urlsplit(doc["ref"]).path.strip("/")
        require((public / route / "index.html").is_file(), "search points to a missing page: " + route)
    routes = {urlsplit(ref).path for ref in refs}
    require("/" in routes and "/blog/" in routes, "search omitted section documents")
    for path in (source / "content/errors").rglob("index.md"):
        match = re.search(r'^path = "([^"]+)"$', path.read_text(), re.MULTILINE)
        require(match is not None, "error source has no explicit path: " + str(path))
        if match:
            route = "/" + match[1].strip("/") + "/"
            require((public / route.strip("/") / "index.html").is_file(), "missing compiler error route: " + route)
            require(route in routes, "search omitted compiler error route: " + route)

    blog = source / "content/blog"
    dated_posts = []
    for path in blog.glob("*.md"):
        match = re.search(r"^date\s*=\s*(\d{4}-\d{2}-\d{2})", path.read_text(), re.MULTILINE)
        if match:
            dated_posts.append((match[1], "/blog/" + path.stem + "/"))
    dated_posts.sort(reverse=True)
    expected = [route for _, route in dated_posts]
    for name in ("rss.xml", "blog/rss.xml"):
        channel = ET.parse(public / name).getroot().find("channel")
        require(channel is not None, name + " has no RSS channel")
        if channel is None:
            continue
        require(bool(channel.findtext("description")), name + " lost its channel description")
        items = channel.findall("item")
        require([urlsplit(item.findtext("link", "")).path for item in items] == expected,
                name + " has incorrect dated-post membership or ordering")
        for item in items:
            require(bool(item.findtext("guid")), name + " item has no stable GUID")
            require(bool(item.findtext("description")), name + " item has no content or summary")
            try:
                parsedate_to_datetime(item.findtext("pubDate", ""))
            except (TypeError, ValueError):
                failures.append(name + " item has invalid RFC 2822 pubDate")

    first_page = Page(public / "blog/page/1/index.html")
    require(bool(first_page.refresh) and "/blog/" in first_page.refresh,
            "blog/page/1 is not a redirect to the canonical listing")
    sitemap = ET.parse(public / "sitemap.xml")
    locations = {node.text for node in sitemap.findall(".//{http://www.sitemaps.org/schemas/sitemap/0.9}loc")}
    require(set(refs).issubset(locations), "sitemap omits source page URLs")
    return failures


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("public", type=Path)
    parser.add_argument("--source", type=Path, default=Path(__file__).resolve().parent.parent / "website")
    args = parser.parse_args()
    try:
        failures = check(args.public, args.source)
    except (OSError, ValueError, KeyError, ET.ParseError) as error:
        failures = [str(error)]
    if failures:
        print("Website verification failed:\n" + "\n".join("  " + item for item in failures), file=sys.stderr)
        return 1
    print("Website verified: routes, data, excerpts, highlighting, search documents, pagination, feeds and sitemap")
    return 0


if __name__ == "__main__":
    sys.exit(main())
