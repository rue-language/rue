// The runtime-performance dashboard (ADR-0072 Decision 8).
//
// Reads the `runtime` section of /performance-data.json, which `rue-bench
// derive` rebuilds from the raw runtime records at site build time. Nothing
// here recomputes a statistic: medians, dispersions, segment membership, and
// the flagging verdict all arrive already decided, because a second
// implementation of "did this move" would drift from the one the calibrator
// will use — and the drifted one would be the one declaring regressions.
//
// The invariant this file exists to protect:
//
//   A raw median is comparable only within a segment — a run of consecutive
//   points that agree on both the input's identity and the program's own source
//   identity. Never draw a line across a boundary, never take a delta across
//   one, and never let a trailing window reach through one. The underlying
//   input changed, so the numbers are not on the same scale, and a continuous
//   line across it is a picture of a trend that did not happen.
(function () {
  "use strict";

  var SVG = "http://www.w3.org/2000/svg";

  // One series, so one data ink — the same green the compiler dashboard plots
  // with, because these two pages are siblings and a second green would read as
  // a second meaning. Discontinuities are marked with the site's own gold
  // accent rather than a second categorical hue: they are not another series.
  //
  // Every pair here is computed, not eyeballed. Against the green, gold
  // separates by ΔE 10.1 (protan, light surface) and 18.7 (dark), and the flag
  // ink by 9.9 (deutan, both) — all above the ΔE 8 target. The amber this page
  // first used for flags separated by only 4.2, which is why it is not here.
  //
  // Two results are honestly imperfect and relieved rather than hidden. Gold
  // falls below 3:1 against the cream surface and the flag ink below 3:1
  // against the dark one, so neither may be the sole carrier of meaning: every
  // discontinuity rule and every flagged point is labelled in a text ink, the
  // cards repeat the flag as a word, and the segments table repeats the whole
  // series in words. And gold in dark mode sits at OKLCH L 0.788, above the
  // 0.48–0.67 band a categorical slot should hold; it is the site's own accent
  // token used as an annotation rule rather than a series colour, and too light
  // for the band on a dark surface means more contrast, not less.
  var SERIES = "#228833";
  var BREAK = "var(--color-gold)";
  var FLAG = "#aa3377";
  var GRID = "var(--color-border)";
  var LABEL = "var(--color-muted)";

  function element(name, attributes) {
    var node = document.createElementNS(SVG, name);
    Object.keys(attributes).forEach(function (key) {
      node.setAttribute(key, attributes[key]);
    });
    return node;
  }

  function text(attributes, content) {
    var node = element("text", attributes);
    node.textContent = content;
    return node;
  }

  function escapeHtml(value) {
    var holder = document.createElement("div");
    holder.textContent = value;
    return holder.innerHTML;
  }

  function shortHash(value) { return (value || "").slice(0, 12); }

  // Client coordinates into the chart's own viewBox. The chart is drawn at a
  // fixed viewBox and scaled to its container, so on a narrow screen the
  // rendered box and the coordinate system disagree.
  function svgLocation(svg, event) {
    var matrix = svg.getScreenCTM();
    if (!matrix) { return null; }
    var point = svg.createSVGPoint();
    point.x = event.clientX;
    point.y = event.clientY;
    return point.matrixTransform(matrix.inverse());
  }

  function fmtWhen(value) {
    var parts = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(value || "");
    return parts ? parts[1] + " " + parts[2] + " UTC" : (value || "unknown time");
  }

  function fmtDay(value) {
    var parts = /^(\d{4}-\d{2}-\d{2})/.exec(value || "");
    return parts ? parts[1] : "";
  }

  // These programs run for seconds; the compiler suites' fixed milliseconds
  // would print four leading zeros or six leading digits depending on the
  // workload. One unit is chosen per figure and named, never guessed at by the
  // reader.
  function unitFor(ns) {
    if (ns >= 1e9) { return { divisor: 1e9, suffix: " s" }; }
    if (ns >= 1e6) { return { divisor: 1e6, suffix: " ms" }; }
    if (ns >= 1e3) { return { divisor: 1e3, suffix: " µs" }; }
    return { divisor: 1, suffix: " ns" };
  }

  function fmtIn(unit, ns) {
    return (ns / unit.divisor).toFixed(unit.divisor === 1 ? 0 : 2) + unit.suffix;
  }

  function fmtTime(ns) { return fmtIn(unitFor(ns), ns); }

  function fmtBytes(bytes) {
    if (bytes >= 1048576) { return (bytes / 1048576).toFixed(1) + " MiB"; }
    if (bytes >= 1024) { return (bytes / 1024).toFixed(1) + " KiB"; }
    return bytes + " B";
  }

  function fmtPercent(from, to) {
    if (!from) { return "n/a"; }
    var delta = (to / from - 1) * 100;
    return (delta >= 0 ? "+" : "") + delta.toFixed(2) + "%";
  }

  // Both halves are interpolated into an href and `escapeHtml` does not escape
  // quotes, so each is checked against a shape that cannot carry one rather than
  // trusted for coming from the build.
  var COMMIT = /^[0-9a-f]{7,64}$/;
  var URL_PREFIX = /^https:\/\/[\w.-]+\/[\w./-]*$/;

  function commitLink(data, commit) {
    var label = escapeHtml(shortHash(commit));
    var prefix = data.commit_url_prefix;
    if (typeof prefix !== "string" || !URL_PREFIX.test(prefix) || !COMMIT.test(commit)) {
      return label;
    }
    return '<a href="' + escapeHtml(prefix + commit) +
      '" target="_blank" rel="noopener noreferrer">' + label + "</a>";
  }

  function commitHeading(data, commit) {
    var subject = (data.commit_subjects || {})[commit];
    return "<strong>" + commitLink(data, commit) + "</strong>" +
      (subject ? " — " + escapeHtml(subject) : " (subject unavailable)");
  }

  // Reported only when runs were actually skipped, so an adjacent pair says
  // nothing. Ordinals are positions on trunk's first-parent line resolved at
  // site build time; a commit the build could not place has none, and an
  // unknown gap is left unstated rather than guessed at.
  function skippedLine(data, previousCommit, commit) {
    var ordinals = data.commit_ordinals || {};
    var from = ordinals[previousCommit];
    var to = ordinals[commit];
    if (typeof from !== "number" || typeof to !== "number") { return null; }
    var distance = to - from;
    if (distance <= 1) { return null; }
    var skipped = distance - 1;
    return distance + " commits landed since the previous measurement; " +
      skipped + (skipped === 1 ? " was" : " were") + " not measured.";
  }

  // Only touch the DOM when the text actually changes: the tooltip is an
  // aria-live region, and rewriting it on every pointer sample would have a
  // screen reader announce the same commit continuously.
  function setTooltip(node, lines) {
    var html = lines.join("<br>");
    if (node.innerHTML !== html) { node.innerHTML = html; }
  }

  var root = document.getElementById("rt-root");
  var empty = document.getElementById("rt-empty");
  var emptyReason = document.getElementById("rt-empty-reason");

  function showEmpty(reason) {
    emptyReason.textContent = reason;
    empty.hidden = false;
  }

  // Reports that were collected and refused. This is the loud path, and it
  // exists because the quiet one is a specific predicted failure: ADR-0072
  // Decision 3 warns that site content stepping outside the workload's subset
  // breaks the runtime leg, and if that happens to *every* report there are no
  // points, so an empty page would report that nothing was ever measured. The
  // opposite is true — everything was measured and everything was rejected —
  // and that is the single most useful thing to see.
  function showRejected(rejected) {
    var block = document.getElementById("rt-empty-rejected");
    var list = document.getElementById("rt-empty-rejected-list");
    var summary = document.getElementById("rt-empty-rejected-summary");
    if (!rejected || !rejected.length) { return false; }
    summary.textContent = rejected.length +
      " collected report(s) were refused and none was published, so this page has no " +
      "series to draw. This is a collection failure, not an absence of measurement.";
    list.textContent = "";
    rejected.forEach(function (report) {
      var item = document.createElement("li");
      item.textContent = (report.platform || "unreadable record") +
        (report.epoch ? " epoch " + report.epoch : "") + " " +
        shortHash(report.commit) + ": " + report.reasons.join("; ");
      list.appendChild(item);
    });
    block.hidden = false;
    empty.hidden = false;
    return true;
  }

  fetch("/performance-data.json")
    .then(function (response) { return response.json(); })
    .then(render)
    .catch(function () {
      showEmpty(
        "The measurement data could not be loaded, so this page has nothing to " +
        "show. The raw records remain on the performance-data-v1 branch."
      );
    });

  function render(data) {
    // `undefined` and "collected nothing" are different states, and the derive
    // step keeps them apart on purpose: the section is absent unless a runtime
    // manifest was named. Saying "nothing measured yet" for a build that never
    // looked would be a claim the page has not earned.
    if (!data.runtime) {
      showEmpty(
        "This build of the site did not derive the runtime series, so there is " +
        "nothing to show here. That is a build configuration, not a measurement: " +
        "no claim is being made about whether runtime data exists."
      );
      return;
    }

    var runtime = data.runtime;
    var platforms = (runtime.platforms || []).filter(function (platform) {
      return (platform.epochs || []).some(function (epoch) {
        return (epoch.points || []).length > 0;
      });
    });
    if (!platforms.length) {
      // Order matters: a store whose every report was refused is not a store
      // with nothing in it, and saying so first is the whole point.
      if (showRejected(runtime.rejected)) {
        showEmpty(
          "No runtime series could be drawn, because every collected report was " +
          "refused. The reasons are below; each one is a report that exists on the " +
          "performance-data-v1 branch and could not enter a series."
        );
        return;
      }
      showEmpty(
        "No runtime measurements have been collected yet. The harness is in " +
        "place; this page fills in once the runtime leg of collection has run " +
        "on trunk."
      );
      return;
    }
    root.hidden = false;

    var select = document.getElementById("rt-platform");
    platforms.forEach(function (platform, index) {
      var option = document.createElement("option");
      option.value = String(index);
      option.textContent = platform.platform;
      select.appendChild(option);
    });

    var state = { platform: 0, workload: null, cursor: null, pinned: false };
    select.value = "0";
    select.addEventListener("change", function () {
      state.platform = Number(select.value);
      state.workload = null;
      state.cursor = null;
      state.pinned = false;
      draw();
    });

    function current() { return platforms[state.platform]; }

    // Every point on the selected platform, tagged with its epoch so a
    // boundary can be drawn rather than flattened away.
    function allPoints() {
      var out = [];
      current().epochs.forEach(function (epoch) {
        (epoch.points || []).forEach(function (point) {
          out.push({ epoch: epoch, point: point });
        });
      });
      return out;
    }

    // Entries for the selected workload, in measurement order, each carrying
    // the workload's own figures. A point that did not publish this workload is
    // a gap, not a discontinuity, so it is simply absent.
    function series() {
      return allPoints()
        .filter(function (entry) { return entry.point.workloads[state.workload]; })
        .map(function (entry) {
          return {
            epoch: entry.epoch,
            point: entry.point,
            workload: entry.point.workloads[state.workload]
          };
        });
    }

    function wall(entry) { return entry.workload.metrics.wall_clock; }

    // The newest point that published any workload, which is not always the
    // newest report: a partial run is stored and publishes nothing for the
    // workload that did not complete. Figures come from here; staleness and
    // completeness still come from the newest report, because a page that
    // reported the last *good* run's timestamp would hide an outage.
    function newestPublishing(points) {
      for (var i = points.length - 1; i >= 0; i--) {
        if (Object.keys(points[i].point.workloads).length) { return points[i].point; }
      }
      return null;
    }

    // Two points are comparable exactly when they share an epoch and a segment.
    // Everything that must not reach across a discontinuity asks this.
    function comparable(left, right) {
      return left.epoch.epoch === right.epoch.epoch &&
        left.workload.segment === right.workload.segment;
    }

    // Interaction is bound once and each redraw swaps in a handler closed over
    // the series it just drew, so a platform change cannot leave a stale
    // closure answering the pointer with retired data.
    var interaction = null;
    (function bindChart(svg) {
      var dragging = false;
      function dispatch(event) { if (interaction) { interaction(event, dragging); } }
      svg.addEventListener("pointerdown", function (event) {
        dragging = true;
        if (svg.setPointerCapture) { svg.setPointerCapture(event.pointerId); }
        dispatch(event);
      });
      svg.addEventListener("pointermove", dispatch);
      ["pointerup", "pointercancel"].forEach(function (name) {
        svg.addEventListener(name, function () { dragging = false; });
      });
      svg.addEventListener("keydown", dispatch);
    })(document.getElementById("rt-chart"));

    function draw() {
      var points = allPoints();
      if (!points.length) { return; }
      if (state.workload === null) {
        // The newest point, but not necessarily the newest *report*: a partial
        // run publishes no median for the workload that did not complete, and
        // the schema supports that explicitly. Selecting from it would leave
        // the page blank while every earlier measurement sat right there.
        var names = [];
        for (var i = points.length - 1; i >= 0 && !names.length; i--) {
          names = Object.keys(points[i].point.workloads);
        }
        state.workload = names.length ? names[0] : null;
      }
      var entries = series();
      drawStatus(entries);
      drawWorkloads(points);
      drawChart(data, entries);
      drawSegments(entries);
      drawComparison(runtime);
      drawNotes();
      drawDisclosures(runtime, entries);
    }

    // 1. Status line.
    function drawStatus(entries) {
      var status = document.getElementById("rt-status");
      var health = document.getElementById("rt-health");
      var all = allPoints();
      var newest = all[all.length - 1].point;
      var parts = [];

      if (entries.length) {
        var latest = entries[entries.length - 1];
        parts.push(state.workload + ": " + fmtTime(wall(latest).median) +
          " on " + current().platform + ".");
        var previous = entries.length > 1 ? entries[entries.length - 2] : null;
        if (previous && comparable(previous, latest)) {
          parts.push("Versus the previous measured commit: " +
            fmtPercent(wall(previous).median, wall(latest).median) + ".");
        } else if (previous) {
          // The honest answer, not a number. The previous point measured a
          // different input or program, so a percentage between the two would
          // describe the discontinuity rather than the compiler.
          parts.push("No comparison with the previous measured commit: it sits " +
            "the other side of a discontinuity.");
        } else {
          parts.push("First measurement of this program.");
        }
      } else {
        parts.push("No published points for this program yet.");
      }

      // Read from the newest point that published anything: a partial newest
      // report has no workloads, and "nothing flagged" derived from it would be
      // a claim about a run that reported nothing.
      var published = newestPublishing(all) || newest;
      var flagged = Object.keys(published.workloads).filter(function (name) {
        return published.workloads[name].flagged === true;
      });
      parts.push(flagged.length ? "Flagged: " + flagged.join(", ") + "." : "Nothing flagged.");
      // A page that stopped advancing must read as stopped. Without the newest
      // timestamp stated, a frozen series looks exactly like a current one, so
      // this is the newest report rather than the newest usable one.
      parts.push("Newest measurement: " + fmtWhen(newest.finished_at) + ".");
      status.textContent = parts.join(" ");

      var notices = [];
      if (!newest.complete) {
        notices.push("the newest report was incomplete, so some programs publish no median from it.");
      }
      if (runtime.rejected && runtime.rejected.length) {
        notices.push(runtime.rejected.length +
          " collected report(s) could not enter a series and are not drawn here, so this " +
          "page may be behind the compiler. See Collection health below.");
      }
      if (notices.length) {
        health.hidden = false;
        health.textContent = "Collection health: " + notices.join(" ");
      } else {
        health.hidden = true;
      }
    }

    // 2. One card per measured program.
    function drawWorkloads(points) {
      var container = document.getElementById("rt-workloads");
      container.textContent = "";

      (runtime.workloads || []).forEach(function (workload) {
        var own = points
          .filter(function (entry) { return entry.point.workloads[workload.id]; })
          .map(function (entry) {
            return { epoch: entry.epoch, point: entry.point, workload: entry.point.workloads[workload.id] };
          });
        if (!own.length) { return; }
        var latest = own[own.length - 1];
        var figures = latest.workload.metrics.wall_clock;
        if (!figures) { return; }

        var selected = state.workload === workload.id;
        // The button is wrapped rather than given `role="listitem"` itself:
        // that role would replace its button role, and the control would stop
        // announcing that it can be pressed.
        var slot = document.createElement("div");
        slot.setAttribute("role", "listitem");
        var card = document.createElement("button");
        card.type = "button";
        card.setAttribute("aria-pressed", String(selected));
        card.style.cssText = "text-align:left;padding:0.75rem;border:1px solid " +
          (selected ? SERIES : "var(--color-border)") +
          ";background:none;cursor:pointer;font:inherit;width:100%;";
        card.addEventListener("click", function () {
          state.workload = workload.id;
          state.cursor = null;
          state.pinned = false;
          draw();
        });

        var previous = own.length > 1 ? own[own.length - 2] : null;
        var delta = !previous
          ? "no prior point"
          : (comparable(previous, latest)
            ? fmtPercent(previous.workload.metrics.wall_clock.median, figures.median) +
              " versus the previous measured commit"
            : "not comparable with the previous measured commit — a discontinuity sits between them");
        var rule = (latest.epoch.flagging || {})[workload.id];
        var badge = latest.workload.flagged === true
          ? " · flagged"
          : (!rule
            ? " · not judged"
            : (latest.workload.flagged === null || latest.workload.flagged === undefined
              ? " · not enough history"
              : ""));
        // Three states, not two. "No bound declared" is not the same as "a
        // provisional bound found nothing", and neither is calibration.
        var posture = !rule
          ? "no bound declared for this program on this platform yet"
          : (latest.workload.flag_posture === "calibrated"
            ? "calibrated"
            : "advisory — a declared bound, not a measured one");

        card.innerHTML =
          '<div style="font-weight:600;">' + escapeHtml(workload.id) + escapeHtml(badge) + "</div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">' +
            escapeHtml(workload.question) + "</div>" +
          '<div style="margin-top:0.4rem;">' + fmtTime(figures.median) +
          ' <span style="color:var(--color-muted);">± ' + fmtTime(figures.mad) +
          " MAD over " + figures.count + " samples</span></div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">Δ ' +
            escapeHtml(delta) + "</div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">Flagging: ' +
            escapeHtml(posture) + "</div>";
        slot.appendChild(card);
        container.appendChild(slot);
      });
    }

    // 3. Wall time over commits, one stretch per segment.
    function drawChart(data, entries) {
      var svg = document.getElementById("rt-chart");
      var caption = document.getElementById("rt-chart-caption");
      var tooltip = document.getElementById("rt-tooltip");
      svg.textContent = "";
      document.getElementById("rt-selected").textContent = state.workload || "—";

      if (!entries.length) {
        caption.textContent = "No published observations for this program.";
        interaction = null;
        setTooltip(tooltip, ["No observations for this program to describe."]);
        return;
      }

      // Zero-based, and tall enough for the dispersion bars. A floor chosen to
      // fill the box would make hosted-runner noise look like a trend.
      var hi = Math.max.apply(null, entries.map(function (entry) {
        return wall(entry).median + wall(entry).mad;
      })) * 1.12 || 1;
      var unit = unitFor(hi);
      var x = function (i) { return 48 + (i / Math.max(entries.length - 1, 1)) * 832; };
      var y = function (v) { return 210 - (v / hi) * 186; };

      // Hairline grid, one shade off the surface, with the axis labelled in a
      // text ink rather than in the series colour.
      for (var step = 0; step <= 4; step++) {
        var value = (hi / 4) * step;
        svg.appendChild(element("line", {
          x1: 48, y1: y(value), x2: 880, y2: y(value), stroke: GRID, "stroke-width": 1
        }));
        svg.appendChild(text({
          x: 44, y: y(value) + 4, fill: LABEL, "font-size": 11, "text-anchor": "end"
        }, fmtIn(unit, value)));
      }

      // Stretches, split wherever a comparison would be invalid. This is the
      // whole point of the chart: the gap is the finding.
      var stretch = [];
      var breaks = [];
      entries.forEach(function (entry, i) {
        if (i > 0 && !comparable(entries[i - 1], entry)) {
          svg.appendChild(element("polyline", {
            points: stretch.join(" "), fill: "none", stroke: SERIES, "stroke-width": 2
          }));
          stretch = [];
          breaks.push({ at: (x(i - 1) + x(i)) / 2, from: entries[i - 1], to: entry });
        }
        stretch.push(x(i) + "," + y(wall(entry).median));
      });
      svg.appendChild(element("polyline", {
        points: stretch.join(" "), fill: "none", stroke: SERIES, "stroke-width": 2
      }));

      breaks.forEach(function (boundary) {
        svg.appendChild(element("line", {
          x1: boundary.at, y1: 24, x2: boundary.at, y2: 210,
          stroke: BREAK, "stroke-width": 1.5, "stroke-dasharray": "4 3"
        }));
        var label = boundary.from.epoch.epoch !== boundary.to.epoch.epoch
          ? "epoch " + boundary.to.epoch.epoch
          : (boundary.from.workload.fixture_identity !== boundary.to.workload.fixture_identity
            ? "input changed"
            : "program changed");
        svg.appendChild(text({
          x: boundary.at + 4, y: 20, fill: LABEL, "font-size": 11
        }, label));
      });

      // Dispersion, drawn as the sampled range around each median rather than
      // implied by the line's smoothness.
      entries.forEach(function (entry, i) {
        var figures = wall(entry);
        if (!figures.mad) { return; }
        svg.appendChild(element("line", {
          x1: x(i), y1: y(figures.median + figures.mad),
          x2: x(i), y2: y(Math.max(figures.median - figures.mad, 0)),
          stroke: SERIES, "stroke-width": 1, "stroke-opacity": 0.45
        }));
      });

      entries.forEach(function (entry, i) {
        svg.appendChild(element("circle", {
          cx: x(i), cy: y(wall(entry).median), r: 3,
          fill: SERIES, "pointer-events": "none"
        }));
        // Flagged points are direct-labelled as well as ringed: a status that
        // reached the reader only as a colour would not reach every reader.
        if (entry.workload.flagged === true) {
          svg.appendChild(element("circle", {
            cx: x(i), cy: y(wall(entry).median), r: 6.5,
            fill: "none", stroke: FLAG, "stroke-width": 2, "pointer-events": "none"
          }));
          // The word wears a text ink, not the ring's: it has to stay legible
          // on both surfaces, and the ring beside it is what carries identity.
          // Clamped inside the drawing, since the last point is on the right
          // edge and a centred label there would be cut in half by the viewBox.
          svg.appendChild(text({
            x: Math.max(70, Math.min(858, x(i))),
            y: Math.max(y(wall(entry).median) - 12, 34), fill: "currentColor",
            "font-size": 10, "text-anchor": "middle"
          }, "flagged"));
        }
      });

      drawDateAxis(svg, x, entries);

      var highlight = element("circle", {
        r: 8, fill: "none", stroke: "currentColor", "stroke-width": 2, "pointer-events": "none"
      });
      svg.appendChild(highlight);

      function selectMark(position) {
        state.cursor = Math.max(0, Math.min(entries.length - 1, position));
        var entry = entries[state.cursor];
        highlight.setAttribute("cx", x(state.cursor));
        highlight.setAttribute("cy", y(wall(entry).median));
        highlight.setAttribute("fill", state.pinned ? "currentColor" : "none");
        drawTooltip(data, entries, state.cursor);
      }

      interaction = function (event, dragging) {
        if (event.type === "keydown") {
          if (event.key === "Escape" && state.pinned) {
            state.pinned = false;
            selectMark(state.cursor);
            event.preventDefault();
          }
          if (event.key === "ArrowRight") { selectMark(state.cursor + 1); event.preventDefault(); }
          if (event.key === "ArrowLeft") { selectMark(state.cursor - 1); event.preventDefault(); }
          return;
        }
        if (event.type === "pointermove" && state.pinned && !dragging) { return; }
        var at = svgLocation(svg, event);
        if (!at) { return; }
        var nearest = Math.round(((at.x - 48) / 832) * (entries.length - 1));
        nearest = Math.max(0, Math.min(entries.length - 1, nearest));
        if (event.type === "pointerdown") {
          state.pinned = !(state.pinned && nearest === state.cursor);
        }
        selectMark(nearest);
      };

      svg.setAttribute("aria-label", "Whole-process wall time for " + state.workload +
        " on " + current().platform +
        ". Use left and right arrow keys to move between measured commits.");
      caption.textContent = entries.length + " measured commit(s) in " +
        (breaks.length + 1) + " comparable stretch(es). A dashed gold rule marks a " +
        "discontinuity — a change of input, of the program's own source, or of the " +
        "measurement epoch — across which the medians either side are not on the " +
        "same scale. The vertical whisker on each point is its median absolute " +
        "deviation." +
        (breaks.length
          ? " All stretches share one axis so their absolute cost is visible, which " +
            "is not an invitation to read across a break: the heights differ because " +
            "the work differs."
          : "");

      selectMark(state.cursor === null
        ? entries.length - 1
        : Math.min(state.cursor, entries.length - 1));
    }

    // Ticks are chosen by position rather than by date so they always land on
    // real measurements: the series is irregular, and a tick at an evenly
    // spaced date would point at a gap.
    function drawDateAxis(svg, x, entries) {
      var wanted = Math.min(entries.length, 5);
      var chosen = [];
      for (var slot = 0; slot < wanted; slot++) {
        var pick = wanted === 1
          ? entries.length - 1
          : Math.round((slot / (wanted - 1)) * (entries.length - 1));
        if (chosen.indexOf(pick) === -1) { chosen.push(pick); }
      }
      chosen.forEach(function (pick, slot) {
        svg.appendChild(element("line", {
          x1: x(pick), y1: 210, x2: x(pick), y2: 214, stroke: GRID
        }));
        svg.appendChild(text({
          x: x(pick), y: 228, fill: LABEL, "font-size": 11,
          // The end labels would otherwise overhang the viewBox and clip.
          "text-anchor": slot === 0 ? "start" : (slot === chosen.length - 1 ? "end" : "middle")
        }, fmtDay(entries[pick].point.finished_at)));
      });
    }

    function drawTooltip(data, entries, index) {
      var tooltip = document.getElementById("rt-tooltip");
      var entry = entries[index];
      var figures = wall(entry);
      var lines = [
        commitHeading(data, entry.point.commit),
        "Measured " + fmtWhen(entry.point.finished_at) + " on " + current().platform +
          ", epoch " + entry.epoch.epoch + ", suite revision " + entry.epoch.suite_revision + ".",
        "Wall time " + fmtTime(figures.median) + " ± " + fmtTime(figures.mad) +
          " MAD over " + figures.count + " fresh processes.",
        "Segment " + entry.workload.segment + ": input " +
          escapeHtml(shortHash(entry.workload.fixture_identity)) +
          " (" + fmtBytes(entry.workload.fixture_bytes) + "), program source " +
          escapeHtml(shortHash(entry.workload.source_identity)) + "."
      ];

      if (index > 0) {
        var previous = entries[index - 1];
        if (comparable(previous, entry)) {
          lines.push("Versus the previous measured commit (" +
            escapeHtml(shortHash(previous.point.commit)) + "): " +
            fmtPercent(wall(previous).median, figures.median) + ".");
        } else {
          lines.push("The previous measured commit (" +
            escapeHtml(shortHash(previous.point.commit)) +
            ") is on the other side of a discontinuity, so no delta is shown: " +
            "the two medians are measurements of different work.");
        }
        var skipped = skippedLine(data, previous.point.commit, entry.point.commit);
        if (skipped) { lines.push(skipped); }
      } else {
        lines.push("First measured commit in this view.");
      }

      lines.push(flaggingLine(entry));

      (entry.epoch.annotations || []).forEach(function (note) {
        if (note.commit !== entry.point.commit) { return; }
        if (note.workload && note.workload !== state.workload) { return; }
        lines.push("Event here — " + escapeHtml(note.kind.replace(/_/g, " ")) + ": " +
          escapeHtml(note.detail) + ".");
      });

      lines.push(state.pinned
        ? "<em>Pinned. Click this commit again, or press Escape, to release.</em>"
        : '<em style="color:var(--color-muted);">Click to pin, then follow the hash to the commit.</em>');
      setTooltip(tooltip, lines);
    }

    // What a flag asserts, stated rather than asserted. The rule's constants
    // come from the epoch, and the posture says whether they were measured or
    // are standing in until they have been.
    function flaggingLine(entry) {
      var rule = (entry.epoch.flagging || {})[state.workload];
      var figures = wall(entry);
      // No bound, no verdict. Stated as an open decision rather than as a
      // reassuring silence, because "not flagged" and "nothing is watching for
      // a flag" are very different things to read.
      if (!rule) {
        return "No movement rule: this epoch declares neither a calibration nor a " +
          "provisional bound for this program, so nothing here is judged. What that " +
          "bound should be is ADR-0072's open question 4, awaiting the dispersion " +
          "this series is accumulating.";
      }
      var posture = entry.workload.flag_posture === "calibrated"
        ? ""
        : " This program's dispersion is not calibrated on this platform, so the " +
          "bound is a declared judgement and the flag is advisory: a triage item, " +
          "never a gate.";
      if (entry.workload.flagged === null || entry.workload.flagged === undefined) {
        return "Movement unknown: the rule needs " + rule.window +
          " prior measurements within this segment and does not have them yet." + posture;
      }
      if (typeof entry.workload.window_median_ns !== "number") {
        return (entry.workload.flagged ? "Flagged." : "Not flagged.") + posture;
      }
      var comparison = fmtTime(figures.median) + " against a trailing " + rule.window +
        "-measurement in-segment median of " + fmtTime(entry.workload.window_median_ns) +
        ", " + fmtPercent(entry.workload.window_median_ns, figures.median);
      return (entry.workload.flagged ? "Flagged: " : "Not flagged: ") + comparison +
        (entry.workload.flagged ? " — beyond " : " — within ") + rule.k +
        "× the combined noise of the two." + posture;
    }

    // 4. The series as a table, segmented. The figures a reader who cannot use
    // the chart needs, and the only place two medians may honestly be compared:
    // within a row, never between them.
    function drawSegments(entries) {
      var body = document.getElementById("rt-segments");
      body.textContent = "";
      if (!entries.length) {
        var none = document.createElement("tr");
        var cell = document.createElement("td");
        cell.colSpan = 6;
        cell.style.color = "var(--color-muted)";
        cell.textContent = "No published observations for this program.";
        none.appendChild(cell);
        body.appendChild(none);
        return;
      }

      var groups = [];
      entries.forEach(function (entry, i) {
        if (i === 0 || !comparable(entries[i - 1], entry)) { groups.push([]); }
        groups[groups.length - 1].push(entry);
      });

      groups.forEach(function (group) {
        var first = group[0];
        var last = group[group.length - 1];
        var row = document.createElement("tr");
        row.innerHTML =
          "<td>" + first.workload.segment +
            ' <span style="color:var(--color-muted);">· epoch ' + first.epoch.epoch + "</span></td>" +
          "<td>" + group.length + "</td>" +
          '<td><code>' + escapeHtml(shortHash(first.workload.fixture_identity)) + "</code> " +
            fmtBytes(first.workload.fixture_bytes) + "</td>" +
          '<td><code>' + escapeHtml(shortHash(first.workload.source_identity)) + "</code></td>" +
          '<td class="figure">' + fmtTime(wall(first).median) +
            (group.length > 1 ? " → " + fmtTime(wall(last).median) : "") + "</td>" +
          '<td class="figure">' + (group.length > 1
            ? fmtPercent(wall(first).median, wall(last).median)
            : '<span style="color:var(--color-muted);">single measurement</span>') + "</td>";
        body.appendChild(row);
      });
    }

    // 5. The cross-tool comparison. The table's captions live in the template,
    // inside the figure, so this function can only ever add rows to a table
    // that already carries them (ADR-0072 Decision 3).
    //
    // Nothing populates it yet: the peer leg needs gazette, a pinned Hugo, and
    // the parity configurations, none of which exist. When it does, its rows
    // arrive as `runtime.comparison` and are rendered here — an empty or absent
    // comparison renders as the honest empty state, never as an estimate.
    function drawComparison(runtime) {
      var figure = document.getElementById("rt-comparison");
      var body = document.getElementById("rt-comparison-rows");
      var emptyNote = document.getElementById("rt-comparison-empty");
      var rows = (runtime.comparison && runtime.comparison.rows) || [];
      body.textContent = "";
      if (!rows.length) {
        figure.hidden = true;
        emptyNote.hidden = false;
        return;
      }
      figure.hidden = false;
      emptyNote.hidden = true;
      rows.forEach(function (entry) {
        var row = document.createElement("tr");
        row.innerHTML =
          "<td>" + escapeHtml(entry.tool || "") + "</td>" +
          "<td>" + escapeHtml(entry.scale || "") + "</td>" +
          "<td>" + escapeHtml(entry.threads || "") + "</td>" +
          '<td class="figure">' + fmtTime(entry.median_ns || 0) + "</td>" +
          '<td class="figure">' + (typeof entry.ratio === "number"
            ? entry.ratio.toFixed(2) + "×" : "—") + "</td>" +
          "<td>" + escapeHtml(entry.version || "") + "</td>";
        body.appendChild(row);
      });
    }

    // 6. Field notes: the annotated events ADR-0072 Decision 2 asks for.
    function drawNotes() {
      var list = document.getElementById("rt-notes");
      list.textContent = "";
      var any = false;
      current().epochs.forEach(function (epoch) {
        (epoch.annotations || []).forEach(function (note) {
          any = true;
          var item = document.createElement("li");
          var subject = note.workload ? note.workload + ": " : "";
          var meaning = note.kind === "compiler_release"
            ? "A new compiler version; the series continues, because the compiler is what it measures."
            : "Measurements either side are not on the same scale, so the series is segmented here.";
          item.textContent = shortHash(note.commit) + " — " + subject +
            note.kind.replace(/_/g, " ") + ", " + note.detail + ". " + meaning;
          list.appendChild(item);
        });
      });
      // Absent rather than empty: the peer leg has never run, so "no peer
      // toolchain bumps" would be a finding the page has not made.
      var pending = document.createElement("li");
      pending.style.color = "var(--color-muted)";
      pending.textContent = any
        ? "Peer toolchain bumps will be annotated here once peer measurement runs."
        : "No annotated events in this window. Peer toolchain bumps will appear " +
          "here once peer measurement runs.";
      list.appendChild(pending);
    }

    // 7. Disclosures.
    function drawDisclosures(runtime, entries) {
      var others = document.getElementById("rt-other-metrics");
      others.textContent = "";
      var newest = newestPublishing(allPoints());
      Object.keys((newest && newest.workloads) || {}).forEach(function (name) {
        var metrics = newest.workloads[name].metrics || {};
        var row = document.createElement("div");
        row.textContent = name + ": " +
          (metrics.peak_memory ? fmtBytes(metrics.peak_memory.median) + " peak RSS" : "no memory figure") +
          ", " +
          (metrics.binary_size ? fmtBytes(metrics.binary_size.median) + " binary" : "no size figure");
        others.appendChild(row);
      });

      var rule = entries.length
        ? (entries[entries.length - 1].epoch.flagging || {})[state.workload]
        : null;
      document.getElementById("rt-flag-rule").textContent = rule
        ? "This epoch judges " + state.workload + " with k = " + rule.k +
          " over a trailing window of " + rule.window + " in-segment measurements (" +
          rule.posture + (rule.reference ? ", " + rule.reference : "") + ")."
        : "No bound is declared for " + state.workload + " on this platform, so no flag " +
          "is published for it — not even an advisory one. That is deliberate rather " +
          "than pending plumbing: ADR-0072's open question 4 asks what threshold suits " +
          "this series once its dispersion is understood, and a number chosen before " +
          "then would be a threshold nobody picked applied to a workload nobody has " +
          "measured. The bound is declared in performance/runtime.toml when there are " +
          "grounds for one.";

      var regime = document.getElementById("rt-regime");
      regime.textContent = "";
      current().epochs.forEach(function (epoch) {
        var row = document.createElement("div");
        row.textContent = "Epoch " + epoch.epoch + ", suite revision " + epoch.suite_revision +
          ": " + epoch.optimization + " release-quality build, " +
          epoch.thread_policy.replace(/_/g, " ") + ", hardware counters " +
          epoch.hardware_counters.replace(/_/g, " ") +
          ". Each sample is one fresh process timed from immediately before spawn " +
          "through exit; fixture preparation and the correctness oracle are both " +
          "outside that window.";
        regime.appendChild(row);
      });

      var rejected = document.getElementById("rt-rejected");
      rejected.textContent = "";
      if (!runtime.rejected || !runtime.rejected.length) {
        rejected.textContent = "No reports were rejected.";
        return;
      }
      runtime.rejected.forEach(function (report) {
        var row = document.createElement("div");
        row.textContent = (report.platform || "unreadable record") +
          (report.epoch ? " epoch " + report.epoch : "") + " " +
          shortHash(report.commit) + ": " + report.reasons.join("; ");
        rejected.appendChild(row);
      });
    }

    draw();
  }
})();
