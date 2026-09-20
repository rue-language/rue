#!/usr/bin/env python3
"""Exercise Gazette's production features through its real build command.

Usage: python3 scripts/test-gazette-production.py /absolute/path/to/gazette

Each test creates a small independent site. Expected routes, document text,
feed metadata, and rejection behavior are asserted directly; no other site
generator or production website checkout is needed.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET
from datetime import date, timedelta
from email.utils import parsedate_to_datetime
from html import unescape
from html.parser import HTMLParser
from pathlib import Path


class Document(HTMLParser):
    """Observe rendered text independently of markup and entity spellings."""

    def __init__(self, source: str):
        super().__init__(convert_charrefs=True)
        self.ids: dict[str, str] = {}
        self.links: list[str] = []
        self.classes: set[str] = set()
        self.raw: dict[str, list[str]] = {}
        self.stack: list[tuple[str, str | None]] = []
        self.regions: list[tuple[str, int]] = []
        self.feed(source)
        self.close()

    def handle_starttag(self, tag, attrs):
        fields = dict(attrs)
        identifier = fields.get("id")
        if identifier is not None:
            self.ids.setdefault(identifier, "")
        self.classes.update(fields.get("class", "").split())
        if tag == "a":
            self.links.append(fields.get("href", ""))
        if tag in {"pre", "script", "style", "textarea"}:
            blocks = self.raw.setdefault(tag, [])
            self.regions.append((tag, len(blocks)))
            blocks.append("")
        if tag not in {"area", "base", "br", "col", "embed", "hr", "img",
                       "input", "link", "meta", "param", "source", "track", "wbr"}:
            self.stack.append((tag, identifier))

    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, -1, -1):
            if self.stack[index][0] == tag:
                del self.stack[index:]
                break
        if self.regions and self.regions[-1][0] == tag:
            self.regions.pop()

    def handle_data(self, data):
        for _, identifier in self.stack:
            if identifier is not None:
                self.ids[identifier] += data
        for tag, index in self.regions:
            self.raw[tag][index] += data


class ProductionTests(unittest.TestCase):
    binary: Path

    def setUp(self):
        temporary = tempfile.TemporaryDirectory(prefix="gazette-production-")
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name)

    def write(self, site: Path, path: str, contents: str):
        target = site / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(contents, encoding="utf-8")

    def site(self, name="site", config="") -> Path:
        site = self.root / name
        self.write(site, "config.toml", (
            'base_url = "https://example.test"\n'
            'title = "Review"\n'
            'description = "A small test site"\n' + config
        ))
        self.write(site, "content/_index.md", (
            '+++\ntitle = "Root"\ntemplate = "index.html"\n+++\n'
        ))
        self.write(site, "templates/index.html", "<p>Root</p>")
        self.write(site, "templates/section.html", "{{ section.content | safe }}")
        self.write(site, "templates/page.html", "{{ page.content | safe }}")
        return site

    def build(self, site: Path, success=True, base_url=None) -> Path:
        output = site / "out"
        command = [str(self.binary), "build", str(site), "-o", str(output)]
        if base_url is not None:
            command.extend(["--base-url", base_url])
        result = subprocess.run(
            command,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            check=False,
        )
        details = result.stdout + result.stderr
        if success:
            self.assertEqual(result.returncode, 0, details)
        else:
            self.assertNotEqual(result.returncode, 0, details)
        return output

    def read(self, output: Path, path="index.html") -> str:
        target = output / path
        self.assertTrue(target.is_file(), "missing output: " + path)
        return target.read_text(encoding="utf-8")

    def test_json_numbers_keep_numeric_semantics_and_strings_stay_strings(self):
        site = self.site()
        data = {
            "zero": 0, "decimal_zero": 0.0, "negative_zero": -0.0,
            "small": 2.5, "large": 10.5, "negative": -2.75,
            "ten": 10, "two": 2, "tiny": 1e-5,
            "string_zero": "0", "string_e": "e", "string_dash": "-",
            "escaped": '<tag> & "quoted" / café — 😀',
        }
        self.write(site, "data.json", json.dumps(data))
        self.write(site, "templates/index.html", """
{% set d = load_data(path="data.json", format="json") %}
<p id="truth">{% if d.zero %}bad{% endif %}{% if d.decimal_zero %}bad{% endif %}{% if d.negative_zero %}bad{% endif %}{% if d.negative %}negative{% endif %}|{% if d.string_zero %}zero{% endif %}|{% if d.string_e %}e{% endif %}|{% if d.string_dash %}dash{% endif %}</p>
<p id="order">{% if d.ten > d.two %}integer{% endif %}|{% if d.small < d.large %}decimal{% endif %}|{% if d.negative < d.zero %}negative{% endif %}|{% if d.decimal_zero == d.zero %}zero{% endif %}|{% if d.negative_zero == d.zero %}negative-zero{% endif %}|{% if d.tiny < d.small %}exponent{% endif %}</p>
<p id="values">{{ d.small }}|{{ d.large }}|{{ d.negative }}</p>
<pre id="escaped">{{ d.escaped }}</pre>
""")
        html = self.read(self.build(site))
        document = Document(html)
        self.assertEqual(document.ids["truth"], "negative|zero|e|dash")
        self.assertEqual(document.ids["order"],
                         "integer|decimal|negative|zero|negative-zero|exponent")
        self.assertEqual(document.ids["values"], "2.5|10.5|-2.75")
        self.assertEqual(document.ids["escaped"], data["escaped"])
        self.assertNotIn("<tag>", html)

    def test_template_arrays_tests_and_arithmetic(self):
        site = self.site()
        self.write(site, "templates/index.html", """
{% set values = ["alpha", "beta"] %}
<p id="values">{% for v in values %}{% if v is starting_with("a") %}{{ v }}{% endif %}{% endfor %}</p>
<p id="math">{{ 2 + 3 * 4 }}|{{ 5 - 0 }}|{% if 3 < 2 + 2 %}yes{% endif %}</p>
""")
        document = Document(self.read(self.build(site)))
        self.assertEqual(document.ids["values"], "alpha")
        self.assertEqual(document.ids["math"], "14|5|yes")

    def test_invalid_arithmetic_fails_instead_of_trapping_or_wrapping(self):
        expressions = ["5 / 0", "5 % 0", "0 - 1", "18446744073709551615 + 1",
                       "18446744073709551615 * 2"]
        for index, expression in enumerate(expressions):
            with self.subTest(expression=expression):
                site = self.site("arithmetic-%d" % index)
                self.write(site, "templates/index.html", "{{ " + expression + " }}")
                output = site / "out"
                result = subprocess.run(
                    [str(self.binary), "build", str(site), "-o", str(output)],
                    stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                    text=True, encoding="utf-8", check=False,
                )
                self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
                self.assertIn("template:", result.stdout + result.stderr)

    def paginated_site(self, count: int, name="site", feeds=False) -> Path:
        config = 'generate_feeds = true\nfeed_filenames = ["rss.xml"]\n' if feeds else ""
        site = self.site(name, config)
        self.write(site, "content/blog/_index.md", (
            '+++\ntitle = "Blog"\nsort_by = "date"\npaginate_by = 10\n'
            + ('generate_feeds = true\n' if feeds else "")
            + 'template = "blog.html"\n+++\n'
        ))
        self.write(site, "templates/blog.html", (
            '<div id="posts">{% for p in paginator.pages %}[{{ p.title }}]{% endfor %}</div>'
            '{% if paginator.previous %}<a href="{{ paginator.previous }}">previous</a>{% endif %}'
            '{% if paginator.next %}<a href="{{ paginator.next }}">next</a>{% endif %}'
        ))
        self.write(site, "content/about.md", '+++\ntitle = "Undated"\n+++\nAbout.\n')
        for day in range(1, count + 1):
            self.write(site, "content/blog/post-%02d.md" % day, (
                '+++\ntitle = "post-%02d"\ndate = 2026-09-%02d\n+++\nBody %d.\n'
                % (day, day, day)
            ))
        return site

    def test_pagination_and_rss_use_all_dated_posts_in_order(self):
        site = self.paginated_site(12, feeds=True)
        output = self.build(site)
        first = Document(self.read(output, "blog/index.html"))
        second = Document(self.read(output, "blog/page/2/index.html"))
        self.assertEqual(first.ids["posts"],
                         "".join("[post-%02d]" % day for day in range(12, 2, -1)))
        self.assertEqual(second.ids["posts"], "[post-02][post-01]")
        self.assertEqual(first.links, ["https://example.test/blog/page/2/"])
        self.assertEqual(second.links, ["https://example.test/blog/"])
        redirect = unescape(self.read(output, "blog/page/1/index.html"))
        self.assertIn("https://example.test/blog/", redirect)
        self.assertNotIn("[post-", redirect)
        for path, title in [("rss.xml", "Review"), ("blog/rss.xml", "Review - Blog")]:
            with self.subTest(feed=path):
                channel = ET.fromstring(self.read(output, path)).find("channel")
                self.assertIsNotNone(channel)
                self.assertEqual(channel.findtext("title"), title)
                self.assertEqual(channel.findtext("description"), "A small test site")
                items = channel.findall("item")
                self.assertEqual([item.findtext("title") for item in items],
                                 ["post-%02d" % day for day in range(12, 0, -1)])
                for day, item in zip(range(12, 0, -1), items):
                    expected = "https://example.test/blog/post-%02d/" % day
                    self.assertEqual(item.findtext("link"), expected)
                    self.assertEqual(item.findtext("guid"), expected)
                    published = parsedate_to_datetime(item.findtext("pubDate"))
                    self.assertEqual(published.date(), date(2026, 9, day))
                    self.assertEqual(published.utcoffset(), timedelta(0))
                    self.assertIn("Body %d." % day, item.findtext("description", ""))

    def test_pagination_with_three_pages_and_an_empty_section(self):
        site = self.paginated_site(23, "three-pages")
        output = self.build(site)
        expected = [
            ("blog/index.html", range(23, 13, -1), ["https://example.test/blog/page/2/"]),
            ("blog/page/2/index.html", range(13, 3, -1),
             ["https://example.test/blog/", "https://example.test/blog/page/3/"]),
            ("blog/page/3/index.html", range(3, 0, -1), ["https://example.test/blog/page/2/"]),
        ]
        for path, days, links in expected:
            with self.subTest(page=path):
                document = Document(self.read(output, path))
                self.assertEqual(document.ids["posts"], "".join("[post-%02d]" % day for day in days))
                self.assertEqual(document.links, links)
        self.assertFalse((output / "blog/page/4/index.html").exists())
        redirect = unescape(self.read(output, "blog/page/1/index.html"))
        self.assertIn("https://example.test/blog/", redirect)
        self.assertNotIn("[post-", redirect)

        empty = self.build(self.paginated_site(0, "empty"))
        document = Document(self.read(empty, "blog/index.html"))
        self.assertEqual(document.ids["posts"], "")
        self.assertEqual(document.links, [])
        self.assertIn("https://example.test/blog/",
                      unescape(self.read(empty, "blog/page/1/index.html")))
        self.assertFalse((empty / "blog/page/2/index.html").exists())

    def test_section_feed_skips_undated_pages(self):
        site = self.site()
        self.write(site, "content/notes/_index.md", (
            '+++\ntitle = "Notes"\ngenerate_feeds = true\n+++\n'
        ))
        self.write(site, "content/notes/undated.md", '+++\ntitle = "Undated"\n+++\nNot a feed item.\n')
        self.write(site, "content/notes/dated.md", (
            '+++\ntitle = "Dated"\ndate = 2026-09-20\n+++\nA feed item.\n'
        ))
        feed = ET.fromstring(self.read(self.build(site), "notes/rss.xml"))
        items = feed.findall("channel/item")
        self.assertEqual([item.findtext("title") for item in items], ["Dated"])
        self.assertIn("A feed item.", items[0].findtext("description", ""))

    def test_metadata_and_search_inventory(self):
        site = self.paginated_site(12, feeds=True)
        config = (site / "config.toml").read_text(encoding="utf-8")
        self.write(site, "config.toml", config + (
            'generate_metadata = true\nbuild_search_index = true\n'
            '[search]\nindex_format = "gazette_json"\n'
        ))
        self.write(site, "content/archive/_index.md", (
            '+++\ntitle = "Archive"\nredirect_to = "blog"\n+++\n'
        ))
        self.write(site, "content/about.md", (
            '+++\ntitle = "Quoted \\"Title\\" café"\n'
            'description = "A description"\n+++\nQuotes & symbols.\n'
        ))
        output = self.build(site)
        documents = json.loads(self.read(output, "search_index.en.json"))["documents"]
        indexed = {document["ref"]: document for document in documents}
        content_urls = {
            "https://example.test/", "https://example.test/blog/", "https://example.test/about/",
        } | {"https://example.test/blog/post-%02d/" % day for day in range(1, 13)}
        self.assertEqual(set(indexed), content_urls)
        about = indexed["https://example.test/about/"]
        self.assertEqual(about["title"], 'Quoted "Title" café')
        self.assertEqual(about["description"], "A description")
        self.assertEqual(about["path"], "https://example.test/about/")
        self.assertIn("Quotes &amp; symbols.", about["body"])
        sitemap = ET.fromstring(self.read(output, "sitemap.xml"))
        locations = {node.text for node in sitemap.iter() if node.tag.endswith("}loc")}
        self.assertEqual(locations, content_urls | {
            "https://example.test/archive/", "https://example.test/blog/page/1/",
            "https://example.test/blog/page/2/",
        })
        self.assertIn("Sitemap: https://example.test/sitemap.xml", self.read(output, "robots.txt"))
        self.assertIn("Not found", self.read(output, "404.html"))
        self.assertEqual(self.read(output, ".nojekyll"), "")

    def test_base_url_override_reaches_templates_content_and_metadata(self):
        site = self.site(config=(
            'generate_feeds = true\ngenerate_metadata = true\nbuild_search_index = true\n'
            '[search]\nindex_format = "gazette_json"\n'
        ))
        self.write(site, "content/_index.md", (
            '+++\ntitle = "Root"\ntemplate = "index.html"\n+++\n[post](@/post.md)\n'
        ))
        self.write(site, "content/post.md", (
            '+++\ntitle = "Post"\ndate = 2026-09-20\n+++\n[home](@/_index.md)\n'
        ))
        self.write(site, "templates/index.html", (
            '<p id="config">{{ config.base_url }}</p>'
            '<a href="{{ get_url(path=\'asset.css\') }}">asset</a>'
            '<a href="{{ get_url(path=\'@/post.md\') }}">post</a>'
            '{{ section.content | safe }}'
        ))
        self.write(site, "templates/page.html", (
            '<p id="permalink">{{ page.permalink }}</p>{{ page.content | safe }}'
        ))
        local = "http://127.0.0.1:1111"
        output = self.build(site, base_url=local)
        root = Document(self.read(output))
        self.assertEqual(root.ids["config"], local)
        self.assertEqual(root.links, [local + "/asset.css", local + "/post/", local + "/post/"])
        post = Document(self.read(output, "post/index.html"))
        self.assertEqual(post.ids["permalink"], local + "/post/")
        self.assertEqual(post.links, [local + "/"])
        feed = ET.fromstring(self.read(output, "rss.xml"))
        self.assertEqual(feed.findtext("channel/link"), local + "/")
        self.assertEqual(feed.findtext("channel/item/link"), local + "/post/")
        self.assertEqual(feed.findtext("channel/item/guid"), local + "/post/")
        documents = json.loads(self.read(output, "search_index.en.json"))["documents"]
        self.assertEqual({document["ref"] for document in documents}, {local + "/", local + "/post/"})
        self.assertTrue(all(document["path"].startswith(local + "/") for document in documents))
        sitemap = ET.fromstring(self.read(output, "sitemap.xml"))
        locations = {node.text for node in sitemap.iter() if node.tag.endswith("}loc")}
        self.assertEqual(locations, {local + "/", local + "/post/"})
        self.assertIn("Sitemap: " + local + "/sitemap.xml", self.read(output, "robots.txt"))

    def test_bundle_explicit_route_is_shared_by_links(self):
        site = self.site()
        self.write(site, "content/errors/_index.md", '+++\ntitle = "Errors"\n+++\n')
        self.write(site, "content/errors/E0001/index.md", (
            '+++\ntitle = "Error"\npath = "errors/E0001"\n+++\n## Here\n\nError.\n'
        ))
        self.write(site, "content/link.md", (
            '+++\ntitle = "Link"\n+++\n[error](@/errors/E0001/index.md#here)\n'
        ))
        output = self.build(site)
        self.read(output, "errors/E0001/index.html")
        document = Document(self.read(output, "link/index.html"))
        self.assertEqual(document.links, ["https://example.test/errors/E0001/#here"])
        self.assertFalse((output / "errors/e0001/index/index.html").exists())

    def test_invalid_routes_fail_before_writing_any_output(self):
        cases = {
            "parent": ["../escaped"],
            "embedded-parent": ["nested/../../escaped"],
            "collision": ["same", "same"],
            "root-collision": ["/"],
        }
        for name, paths in cases.items():
            with self.subTest(case=name):
                site = self.site(name)
                for index, path in enumerate(paths):
                    self.write(site, "content/page%d.md" % index, (
                        '+++\ntitle = "Page"\npath = "%s"\n+++\nBody.\n' % path
                    ))
                output = self.build(site, success=False)
                self.assertFalse((site / "escaped/index.html").exists())
                self.assertEqual([p for p in output.rglob("*") if p.is_file()], [])

    def test_minification_preserves_raw_text_and_quoted_attributes(self):
        specimens = [
            '<p>Hello <em>there</em> friend.</p><p title="x  y > z">after</p>',
            '<textarea>one  two\n three</textarea>',
            '<script>const closer="</pre>"; const again="x  y";\n// comment\nconst result=42;</script>',
            '<style>p::before { content: "a  b > c"; }\n.x {white-space: pre}</style>',
            '<pre><code>let x = "unfinished\n</code></pre><p>Next.</p><pre><code>let c = \'x\';\n  let value =  1;\n</code></pre>',
            '<p>I\'m a programmer.</p><pre>let c = \'x\';\n  let value =  1;\n</pre>',
        ]
        for index, source in enumerate(specimens):
            with self.subTest(case=index):
                site = self.site("minify-%d" % index, "minify_html = true\n")
                self.write(site, "templates/index.html", source)
                rendered = self.read(self.build(site))
                self.assertEqual(Document(rendered).raw, Document(source).raw)
                if index == 0:
                    self.assertIn("Hello <em>there</em> friend.", rendered)
                    self.assertIn('title="x  y > z"', rendered)

    def test_highlighting_reads_syntax_data_and_emits_configured_themes(self):
        site = self.site(config="""[markdown]
highlight_code = true
highlight_theme = "css"
syntax_definitions = "syntaxes.json"
highlight_themes_css = [
  { theme = "gruvbox-dark", filename = "themes/dark.css" },
  { theme = "gruvbox-light", filename = "themes/light.css" },
]
""")
        definitions = {"demo": {"keywords": ["begin"], "line_comment": "//", "quotes": '"'}}
        self.write(site, "syntaxes.json", json.dumps(definitions))
        self.write(site, "templates/index.html", "{{ section.content | safe }}")
        body = 'begin café "a  b < &"\n// end\n'
        self.write(site, "content/_index.md", (
            '+++\ntitle = "Root"\ntemplate = "index.html"\n+++\n'
            '```demo run\n' + body + '```\n'
        ))
        output = self.build(site)
        html = self.read(output)
        document = Document(html)
        self.assertEqual(document.raw["pre"], [body])
        self.assertIn("g-keyword", document.classes)
        self.assertIn("g-string", document.classes)
        self.assertIn("g-comment", document.classes)
        dark = self.read(output, "themes/dark.css")
        light = self.read(output, "themes/light.css")
        self.assertIn(".g-keyword", dark)
        self.assertIn(".g-keyword", light)
        self.assertNotEqual(dark, light)
        definitions["demo"]["keywords"] = ["different"]
        self.write(site, "syntaxes.json", json.dumps(definitions))
        changed = Document(self.read(self.build(site)))
        self.assertEqual(changed.raw["pre"], [body])
        self.assertNotIn("g-keyword", changed.classes)

    def test_invalid_data_templates_and_configurations_fail(self):
        templates = {
            "missing-json": '{% set d = load_data(path="missing.json", format="json") %}{{ d.value }}',
            "invalid-json": '{% set d = load_data(path="data.json", format="json") %}{{ d.value }}',
            "missing-parenthesis": '{% if "abc" is starting_with("a" %}bad{% endif %}',
            "missing-array-comma": '{% for x in [1 2] %}{{ x }}{% endfor %}',
        }
        for name, template in templates.items():
            with self.subTest(case=name):
                site = self.site(name)
                self.write(site, "templates/index.html", template)
                if name == "invalid-json":
                    self.write(site, "data.json", '{"value":oops}')
                self.build(site, success=False)
        configs = {
            "missing-table-close": '[markdown]\nhighlight_themes_css = [\n{ theme="dark", filename="x.css"\n]\n',
            "missing-table-comma": '[markdown]\nhighlight_themes_css = [{ theme="dark" filename="x.css" }]\n',
            "missing-array-close": '[markdown]\nhighlight_themes_css = [\n{ theme="dark", filename="x.css" }\n',
        }
        for name, config in configs.items():
            with self.subTest(case=name):
                self.build(self.site(name, config), success=False)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("gazette", type=Path, help="the compiled Gazette executable")
    args = parser.parse_args()
    ProductionTests.binary = args.gazette.resolve()
    if not ProductionTests.binary.is_file():
        parser.error("Gazette executable does not exist: " + str(ProductionTests.binary))
    unittest.main(argv=[sys.argv[0]], verbosity=2)


if __name__ == "__main__":
    main()
