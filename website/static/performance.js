// The compiler-performance dashboard (ADR-0067 §11).
//
// Reads /performance-data.json, which `rue-bench derive` rebuilds from the raw
// run objects at site build time. Nothing here recomputes a statistic: the
// flagging rule, the ratios, and the index all arrive already decided, because
// a second implementation of "did this move" would drift from the calibrator's.
//
// Two invariants this file must not break:
//
//   The index is dimensionless. It never shares an axis with milliseconds, and
//   platform indexes are never drawn together — they have independent
//   baselines and comparing them would be meaningless.
//
//   Only the additive bands are stacked. They sum exactly to compiler-root
//   time, which is the whole reason stacking them is honest.
(function () {
  "use strict";

  // Phases in pipeline order, then the two structural buckets. The composition
  // bar and the stacked area share this map, which is what lets the bar serve
  // as the stack's legend rather than needing a second one.
  var BAND_COLOURS = {
    source_discovery_and_parsing: "#4477aa",
    program_construction: "#66ccee",
    semantic_analysis: "#228833",
    cfg_and_optimization: "#ccbb44",
    backend: "#ee6677",
    object_generation: "#aa3377",
    linking: "#999999",
    mixed_parallel: "#777777",
    unattributed: "#cccccc"
  };

  var SVG = "http://www.w3.org/2000/svg";

  function ms(ns) { return ns / 1e6; }
  function fmtMs(ns) { return ms(ns).toFixed(2) + " ms"; }
  function shortHash(commit) { return (commit || "").slice(0, 12); }

  function element(name, attributes) {
    var node = document.createElementNS(SVG, name);
    Object.keys(attributes).forEach(function (key) {
      node.setAttribute(key, attributes[key]);
    });
    return node;
  }

  function escapeHtml(value) {
    var holder = document.createElement("div");
    holder.textContent = value;
    return holder.innerHTML;
  }

  // Client coordinates into a chart's own viewBox. Both charts are drawn at a
  // fixed viewBox and scaled to the container, so on a narrow screen the
  // rendered box and the coordinate system disagree — and with the default
  // `meet` fit the chart is letterboxed vertically, which reading `clientY` as
  // a viewBox coordinate would land in the wrong band.
  function svgLocation(svg, event) {
    var matrix = svg.getScreenCTM();
    if (!matrix) { return null; }
    var point = svg.createSVGPoint();
    point.x = event.clientX;
    point.y = event.clientY;
    return point.matrixTransform(matrix.inverse());
  }

  // "2026-08-01T12:34:56Z" as "2026-08-01 12:34 UTC". ISO order, because these
  // are read next to commit hashes and sorted by eye; the zone is spelled out
  // because every timestamp on this page is UTC and a bare time invites the
  // reader to assume otherwise.
  function fmtWhen(value) {
    var parts = /^(\d{4}-\d{2}-\d{2})T(\d{2}:\d{2})/.exec(value || "");
    return parts ? parts[1] + " " + parts[2] + " UTC" : (value || "unknown time");
  }

  function fmtDay(value) {
    var parts = /^(\d{4}-\d{2}-\d{2})/.exec(value || "");
    return parts ? parts[1] : "";
  }

  // Date ticks under a chart's plotted range. Without them neither chart says
  // *when* anything happened, and a run of points is unreadable as a timeline.
  //
  // Ticks are chosen by position rather than by date so they always land on
  // real measurements: the series is irregular, and a tick at an evenly spaced
  // date would point at a gap.
  function drawDateAxis(svg, x, baseline, entries) {
    if (!entries.length) { return; }
    var wanted = Math.min(entries.length, 5);
    var chosen = [];
    for (var slot = 0; slot < wanted; slot++) {
      var pick = wanted === 1
        ? entries.length - 1
        : Math.round((slot / (wanted - 1)) * (entries.length - 1));
      if (chosen.indexOf(pick) === -1) { chosen.push(pick); }
    }
    chosen.forEach(function (pick, slot) {
      var entry = entries[pick];
      svg.appendChild(element("line", {
        x1: x(entry.at), y1: baseline, x2: x(entry.at), y2: baseline + 4,
        stroke: "#999"
      }));
      var label = element("text", {
        x: x(entry.at), y: baseline + 16, fill: "#666", "font-size": 11,
        // The end labels would otherwise overhang the viewBox and clip.
        "text-anchor": slot === 0 ? "start" : (slot === chosen.length - 1 ? "end" : "middle")
      });
      label.textContent = fmtDay(entry.finished_at);
      svg.appendChild(label);
    });
  }

  function commitHeading(data, commit) {
    var subject = (data.commit_subjects || {})[commit];
    return "<strong>" + escapeHtml(shortHash(commit)) + "</strong>" +
      (subject ? " — " + escapeHtml(subject) : " (subject unavailable)");
  }

  // §11's commits-since-last-measurement, required "when runs were skipped" —
  // so an adjacent pair, a distance of exactly one, reports nothing.
  //
  // Both ordinals are positions on trunk's first-parent line, resolved at site
  // build time; subtracting them counts the trunk commits between the two
  // measurements. A commit the build could not place on that line has no
  // ordinal, and an unknown gap is left unstated rather than guessed at — which
  // is also what a non-positive distance means, since measured commits that are
  // not ancestrally ordered cannot have a gap between them.
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

  // What "flagged" means, stated rather than asserted.
  //
  // The rule (ADR-0067 §5) compares this run's median against the median of the
  // trailing window's medians, and flags when they differ by more than `k`
  // times the two dispersions combined in quadrature. Three consequences the
  // bare word "flagged" hides, and which this line therefore spells out:
  //
  //   It is a noise-relative bound, not a percentage threshold. A 1% move on a
  //   quiet workload can flag where 5% on a noisy one does not.
  //
  //   It is direction-agnostic. A large speedup flags exactly as a regression
  //   does, because the rule takes the absolute difference.
  //
  //   The comparison is against a trailing window, not the previous commit, so
  //   it can disagree with the delta shown directly above it.
  function flaggingLine(epoch, point) {
    var rule = epoch.flagging || {};
    if (point.flagged === null || point.flagged === undefined) {
      return "Movement unknown: the rule needs a full trailing window of " +
        rule.window + " prior runs and does not have one yet.";
    }
    if (typeof point.window_median_ns !== "number") {
      return point.flagged
        ? "Flagged: this exceeds the epoch's flagging rule."
        : "Not flagged: this is within the epoch's flagging rule.";
    }
    var delta = (point.median_ns / point.window_median_ns - 1) * 100;
    var comparison = fmtMs(point.median_ns) + " against a trailing " + rule.window +
      "-run median of " + fmtMs(point.window_median_ns) + ", " +
      Math.abs(delta).toFixed(2) + "% " + (delta >= 0 ? "slower" : "faster");
    return point.flagged
      ? "Flagged — moved: " + comparison + ". That gap is wider than " + rule.k +
        "× the combined noise of the two, in either direction."
      : "Not flagged: " + comparison + ", within " + rule.k +
        "× the combined noise of the two.";
  }

  // Only touch the DOM when the text actually changes. Tooltips are aria-live
  // regions, and rewriting one on every pointer sample would have a screen
  // reader announce the same commit continuously as the cursor moves within a
  // single band.
  function setTooltip(node, lines) {
    var html = lines.join("<br>");
    if (node.innerHTML !== html) { node.innerHTML = html; }
  }

  var root = document.getElementById("perf-root");
  var empty = document.getElementById("perf-empty");

  fetch("/performance-data.json")
    .then(function (response) { return response.json(); })
    .then(render)
    .catch(function () { empty.hidden = false; });

  function render(data) {
    var platforms = (data.platforms || []).filter(function (platform) {
      return platform.epochs.some(function (epoch) { return epoch.points.length > 0; });
    });
    if (platforms.length === 0) {
      empty.hidden = false;
      return;
    }
    root.hidden = false;

    var select = document.getElementById("perf-platform");
    platforms.forEach(function (platform, index) {
      var option = document.createElement("option");
      option.value = String(index);
      option.textContent = platform.platform;
      select.appendChild(option);
    });

    var state = { platform: 0, workload: null, cursor: null, band: null, indexCursor: null };

    select.addEventListener("change", function () {
      state.platform = Number(select.value);
      state.workload = null;
      state.cursor = null;
      state.band = null;
      state.indexCursor = null;
      draw();
    });

    // Chart interaction is bound once, here, and each redraw swaps in a handler
    // closed over the series it just drew. Attaching listeners inside the draw
    // functions instead would stack a new closure on every platform or workload
    // change, leaving stale ones to answer the pointer with retired data.
    //
    // Pointer events rather than mouse events, so one handler serves a hover on
    // a desktop and a tap or drag on a touchscreen. Mouse events alone are why
    // these charts had no working cursor at all on a phone or tablet.
    var interaction = { index: null, phases: null };

    function bindChart(svg, key) {
      function dispatch(event) {
        if (interaction[key]) { interaction[key](event); }
      }
      svg.addEventListener("pointerdown", function (event) {
        // Capture so a drag that wanders off the chart keeps scrubbing, which
        // is the normal way a finger moves across a small screen.
        //
        // Focus is deliberately left to the browser. Calling focus() here would
        // work, but it makes a tap indistinguishable from keyboard focus and
        // draws the focus ring around the chart on every touch.
        if (svg.setPointerCapture) { svg.setPointerCapture(event.pointerId); }
        dispatch(event);
      });
      svg.addEventListener("pointermove", dispatch);
      svg.addEventListener("keydown", dispatch);
    }

    bindChart(document.getElementById("perf-index"), "index");
    bindChart(document.getElementById("perf-phases"), "phases");

    function current() { return platforms[state.platform]; }

    // Points across every epoch, each tagged with its epoch so the page can
    // draw separate stretches instead of flattening the discontinuity away.
    function allPoints() {
      var out = [];
      current().epochs.forEach(function (epoch) {
        epoch.points.forEach(function (point) { out.push({ epoch: epoch, point: point }); });
      });
      return out;
    }

    function draw() {
      var points = allPoints();
      if (!points.length) { return; }
      if (state.workload === null) {
        var names = Object.keys(points[points.length - 1].point.workloads);
        state.workload = names.length ? names[0] : null;
      }
      drawStatus(points);
      drawIndex(data, points);
      drawMultiples(data, points);
      drawPhases(data, points);
      drawNotes();
      drawDisclosures(data, points);
    }

    // 1. Status line.
    function drawStatus(points) {
      var status = document.getElementById("perf-status");
      var health = document.getElementById("perf-health");
      var latest = points[points.length - 1];
      var parts = [];
      var index = latest.point.index && latest.point.index.latency;

      if (index) {
        parts.push("Index " + index.toFixed(3) + " on " + current().platform + ".");
        // "Versus the trailing week" against irregular runs means the most
        // recent point at least a week older, not a fixed offset.
        var cutoff = Date.parse(latest.point.finished_at) - 7 * 864e5;
        var earlier = null;
        for (var i = points.length - 1; i >= 0; i--) {
          if (Date.parse(points[i].point.finished_at) <= cutoff && points[i].point.index) {
            earlier = points[i];
            break;
          }
        }
        if (earlier) {
          var delta = (index / earlier.point.index.latency - 1) * 100;
          parts.push("Versus a week ago: " + (delta >= 0 ? "+" : "") + delta.toFixed(2) + "%.");
        } else {
          parts.push("No point a week old yet to compare against.");
        }
      } else {
        parts.push("No headline index yet: this epoch has no baseline, or the latest run was partial.");
      }

      var flagged = Object.keys(latest.point.workloads).filter(function (name) {
        return latest.point.workloads[name].flagged === true;
      });
      // "Moved" is the honest verb: the rule takes an absolute difference, so
      // this set includes workloads that got faster.
      parts.push(flagged.length
        ? "Moved beyond the noise bound: " + flagged.join(", ") + "."
        : "Nothing moved beyond the noise bound.");
      // When the newest point is stated, a page that stopped advancing reads as
      // stopped. Without it a ten-day-old chart is indistinguishable from a
      // current one, which is exactly how a frozen series went unnoticed.
      parts.push("Newest measurement: " + latest.point.finished_at + ".");
      status.textContent = parts.join(" ");

      // Two different unhealthy states, and the second used to be invisible: a
      // rejected run never reaches a chart, so a series can stop advancing
      // while every point already drawn stays perfectly healthy.
      var notices = [];
      if (!latest.point.complete) {
        notices.push(
          "the latest run was incomplete. Did not complete validly: " +
            latest.point.missing.join(", ") +
            ". No headline point is published from a partial run."
        );
      }
      if (data.rejected && data.rejected.length) {
        notices.push(
          data.rejected.length +
            " collected run(s) could not enter a series and are not drawn here, so this " +
            "chart may be behind the compiler. See Collection health below."
        );
      }
      if (notices.length) {
        health.hidden = false;
        health.textContent = "Collection health: " + notices.join(" ");
      } else {
        health.hidden = true;
      }
    }

    // 2. Headline chart. One polyline per epoch, so a baseline change reads as
    // a labelled boundary rather than as a change in the compiler.
    function drawIndex(data, points) {
      var svg = document.getElementById("perf-index");
      var caption = document.getElementById("perf-index-caption");
      var tooltip = document.getElementById("perf-index-tooltip");
      svg.textContent = "";

      var indexed = points.filter(function (entry) { return entry.point.index; });
      if (!indexed.length) {
        caption.textContent = "No index points yet. A ratio needs a baseline to be a ratio of.";
        interaction.index = null;
        setTooltip(tooltip, ["No index points to describe yet."]);
        return;
      }

      var values = indexed.map(function (entry) { return entry.point.index.latency; }).concat([1]);
      var lo = Math.min.apply(null, values);
      var hi = Math.max.apply(null, values);
      var pad = (hi - lo) * 0.15 || 0.05;
      lo -= pad;
      hi += pad;

      var x = function (i) { return 40 + (i / Math.max(points.length - 1, 1)) * 840; };
      var y = function (v) { return 190 - ((v - lo) / (hi - lo)) * 170; };

      svg.appendChild(element("line", {
        x1: 40, y1: y(1), x2: 880, y2: y(1), stroke: "#999", "stroke-dasharray": "4 3"
      }));
      var baselineLabel = element("text", { x: 4, y: y(1) + 4, fill: "#666", "font-size": 11 });
      baselineLabel.textContent = "1.00";
      svg.appendChild(baselineLabel);

      var byEpoch = {};
      points.forEach(function (entry, i) {
        if (!entry.point.index) { return; }
        var key = entry.epoch.epoch;
        (byEpoch[key] = byEpoch[key] || []).push(x(i) + "," + y(entry.point.index.latency));
      });
      Object.keys(byEpoch).forEach(function (key) {
        svg.appendChild(element("polyline", {
          points: byEpoch[key].join(" "), fill: "none", stroke: "#228833", "stroke-width": 2
        }));
      });

      var previousEpoch = null;
      points.forEach(function (entry, i) {
        if (previousEpoch !== null && entry.epoch.epoch !== previousEpoch) {
          svg.appendChild(element("line", {
            x1: x(i), y1: 20, x2: x(i), y2: 190, stroke: "#b45309", "stroke-dasharray": "2 2"
          }));
          var label = element("text", { x: x(i) + 4, y: 32, fill: "#b45309", "font-size": 11 });
          label.textContent = "epoch " + entry.epoch.epoch;
          svg.appendChild(label);
        }
        previousEpoch = entry.epoch.epoch;

        var drifted = entry.epoch.environment_annotations.some(function (note) {
          return note.commit === entry.point.commit;
        });
        if (drifted) {
          svg.appendChild(element("line", {
            x1: x(i), y1: 20, x2: x(i), y2: 190, stroke: "#4477aa", "stroke-dasharray": "1 4"
          }));
        }
      });

      // One mark per index point, carrying its position in the full series:
      // points are placed by measurement order, so a stretch with skipped runs
      // is not evenly spaced and the nearest mark has to be found by distance.
      var marks = [];
      points.forEach(function (entry, i) {
        if (!entry.point.index) { return; }
        marks.push({ at: i, entry: entry });
        svg.appendChild(element("circle", {
          cx: x(i), cy: y(entry.point.index.latency), r: 2.5,
          fill: "#228833", "pointer-events": "none"
        }));
      });

      drawDateAxis(svg, x, 190, marks.map(function (mark) {
        return { at: mark.at, finished_at: mark.entry.point.finished_at };
      }));

      var highlight = element("circle", {
        r: 5, fill: "none", stroke: "#111", "stroke-width": 2, "pointer-events": "none"
      });
      svg.appendChild(highlight);

      function selectMark(position) {
        state.indexCursor = Math.max(0, Math.min(marks.length - 1, position));
        var mark = marks[state.indexCursor];
        highlight.setAttribute("cx", x(mark.at));
        highlight.setAttribute("cy", y(mark.entry.point.index.latency));
        drawIndexTooltip(data, marks, state.indexCursor);
      }

      interaction.index = function (event) {
        if (event.type === "keydown") {
          if (event.key === "ArrowRight") { selectMark(state.indexCursor + 1); event.preventDefault(); }
          if (event.key === "ArrowLeft") { selectMark(state.indexCursor - 1); event.preventDefault(); }
          return;
        }
        var at = svgLocation(svg, event);
        if (!at) { return; }
        var nearest = 0;
        marks.forEach(function (mark, position) {
          if (Math.abs(x(mark.at) - at.x) < Math.abs(x(marks[nearest].at) - at.x)) {
            nearest = position;
          }
        });
        selectMark(nearest);
      };

      svg.setAttribute("aria-label", "Headline index for " + current().platform +
        ". Use left and right arrow keys to move between measured commits.");

      caption.textContent = indexed.length + " index point(s). Dashed orange marks an epoch " +
        "boundary; dotted blue marks an environment change, across which comparisons are advisory.";

      selectMark(state.indexCursor === null
        ? marks.length - 1
        : Math.min(state.indexCursor, marks.length - 1));
    }

    // Tooltip for the headline chart, per §11. The index is dimensionless, so
    // the value it reports is a ratio and never a duration.
    function drawIndexTooltip(data, marks, position) {
      var tooltip = document.getElementById("perf-index-tooltip");
      var entry = marks[position].entry;
      var value = entry.point.index.latency;

      var lines = [
        commitHeading(data, entry.point.commit),
        "Measured " + fmtWhen(entry.point.finished_at) + ".",
        "Index " + value.toFixed(3) + " against the epoch " + entry.epoch.epoch +
          " baseline. Dimensionless: 1.000 is the baseline, lower is faster."
      ];

      if (position > 0) {
        var previous = marks[position - 1].entry;
        if (previous.epoch.epoch !== entry.epoch.epoch) {
          // Each epoch normalizes against its own baseline, so these two
          // numbers are ratios of different things. Subtracting them would
          // report a baseline change as movement in the compiler, which is the
          // same reason the line is drawn as one stretch per epoch.
          lines.push("The previous measured commit (" +
            escapeHtml(shortHash(previous.point.commit)) + ") is in epoch " +
            previous.epoch.epoch + ", which has its own baseline. The two indexes are " +
            "ratios of different things, so no delta is shown across the boundary.");
        } else {
          var delta = (value / previous.point.index.latency - 1) * 100;
          lines.push("Versus the previous measured commit (" +
            escapeHtml(shortHash(previous.point.commit)) + "): " +
            (delta >= 0 ? "+" : "") + delta.toFixed(2) + "%");
        }
        var skipped = skippedLine(data, previous.point.commit, entry.point.commit);
        if (skipped) { lines.push(skipped); }
      } else {
        lines.push("First index point in this view.");
      }

      var flagged = Object.keys(entry.point.workloads).filter(function (name) {
        return entry.point.workloads[name].flagged === true;
      });
      lines.push(flagged.length
        ? "Moved beyond the noise bound here: " + escapeHtml(flagged.join(", ")) + "."
        : "Nothing moved beyond the noise bound here.");

      var drift = entry.epoch.environment_annotations.filter(function (note) {
        return note.commit === entry.point.commit;
      });
      if (drift.length) {
        lines.push("Environment changed here, so comparisons across it are advisory: " +
          escapeHtml(drift[0].changed.join("; ")));
      }

      setTooltip(tooltip, lines);
    }

    // 5. Small multiples: the full-size per-workload signal the index attenuates.
    function drawMultiples(data, points) {
      var container = document.getElementById("perf-multiples");
      container.textContent = "";
      var latest = points[points.length - 1].point;

      (data.workloads || []).forEach(function (workload) {
        var series = points
          .map(function (entry) { return entry.point.workloads[workload.id]; })
          .filter(Boolean);
        var currentPoint = latest.workloads[workload.id];
        if (!series.length || !currentPoint) { return; }

        var selected = state.workload === workload.id;
        var card = document.createElement("button");
        card.type = "button";
        card.setAttribute("role", "listitem");
        card.setAttribute("aria-pressed", String(selected));
        card.style.cssText = "text-align:left;padding:0.75rem;border:1px solid " +
          (selected ? "#228833" : "#ccc") + ";background:none;cursor:pointer;font:inherit;width:100%;";
        card.addEventListener("click", function () {
          state.workload = workload.id;
          state.cursor = null;
          draw();
        });

        var previous = series.length > 1 ? series[series.length - 2] : null;
        var delta = previous
          ? ((currentPoint.median_ns / previous.median_ns - 1) * 100).toFixed(2) + "%"
          : "no prior point";
        // `null` means the trailing window is not full. Rendering that as
        // "stable" would be the comfortable wrong answer.
        //
        // "moved" rather than "flagged": the rule takes an absolute difference,
        // so this fires on a large speedup too, and the word most readers hear
        // as "regression" is the wrong one for what was measured.
        var flag = currentPoint.flagged === true
          ? " · moved"
          : (currentPoint.flagged === null ? " · not enough history" : "");
        // The full sentence on hover and for assistive technology, so the badge
        // can stay a badge without being a riddle.
        card.title = flaggingLine(points[points.length - 1].epoch, currentPoint);

        card.innerHTML =
          '<div style="font-weight:600;">' + escapeHtml(workload.id) + flag + "</div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">' + escapeHtml(workload.question) + "</div>" +
          '<div style="margin-top:0.4rem;">' + fmtMs(currentPoint.median_ns) +
          ' <span style="color:var(--color-muted);">± ' + fmtMs(currentPoint.mad_ns) + " MAD</span></div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">Δ ' + delta +
          " versus the previous measured commit</div>";
        container.appendChild(card);
      });
    }

    // 3. Stacked area of absolute milliseconds for the selected workload.
    function drawPhases(data, points) {
      var svg = document.getElementById("perf-phases");
      var caption = document.getElementById("perf-phases-caption");
      svg.textContent = "";
      document.getElementById("perf-selected").textContent = state.workload || "—";

      var series = points.filter(function (entry) {
        return entry.point.workloads[state.workload];
      });
      if (!series.length) {
        caption.textContent = "No observations for this workload.";
        interaction.phases = null;
        setTooltip(document.getElementById("perf-tooltip"),
          ["No observations for this workload to describe."]);
        document.getElementById("perf-composition").textContent = "";
        return;
      }

      var hi = Math.max.apply(null, series.map(function (entry) {
        return entry.point.workloads[state.workload].compiler_root_ns;
      })) * 1.1;
      var x = function (i) { return 40 + (i / Math.max(series.length - 1, 1)) * 840; };
      var y = function (v) { return 230 - (v / hi) * 200; };

      // The stack as drawn, retained per column so the pointer can be resolved
      // to a band and not merely to a commit: §11 asks for the hovered band and
      // its value, which needs the cursor's height as well as its position.
      var stacks = series.map(function () { return []; });
      var offsets = series.map(function () { return 0; });
      (data.bands || []).forEach(function (band) {
        var upper = [];
        var lower = [];
        series.forEach(function (entry, i) {
          var value = entry.point.workloads[state.workload].bands_ns[band] || 0;
          lower.push(x(i) + "," + y(offsets[i]));
          stacks[i].push({ band: band, from: offsets[i], to: offsets[i] + value });
          offsets[i] += value;
          upper.push(x(i) + "," + y(offsets[i]));
        });
        svg.appendChild(element("polygon", {
          points: upper.concat(lower.reverse()).join(" "),
          fill: BAND_COLOURS[band] || "#ccc"
        }));
      });

      var scale = element("text", { x: 4, y: 16, fill: "#666", "font-size": 11 });
      scale.textContent = ms(hi).toFixed(1) + " ms";
      svg.appendChild(scale);

      drawDateAxis(svg, x, 230, series.map(function (entry, i) {
        return { at: i, finished_at: entry.point.finished_at };
      }));

      // Drawn once and moved, rather than redrawn. Without them a tap on a
      // touchscreen changes the tooltip with nothing on the chart to say which
      // commit or band it now describes.
      var cursorLine = element("line", {
        y1: 30, y2: 230, stroke: "#111", "stroke-dasharray": "3 2", "pointer-events": "none"
      });
      svg.appendChild(cursorLine);
      var bandMark = element("circle", {
        r: 4, fill: "none", stroke: "#111", "stroke-width": 2, "pointer-events": "none"
      });
      svg.appendChild(bandMark);

      // Bands with no time at this commit are skipped: they are zero-height, so
      // the cursor can never be meaningfully "on" one, and every band below the
      // stack's top would otherwise match at the same coordinate.
      function occupied(i) {
        return stacks[i].filter(function (slice) { return slice.to > slice.from; });
      }

      function bandAt(i, height) {
        var found = null;
        occupied(i).forEach(function (slice) {
          // y() descends as the value ascends, so the band's top edge is the
          // smaller coordinate.
          if (height <= y(slice.from) && height >= y(slice.to)) { found = slice.band; }
        });
        return found;
      }

      function moveCursor() {
        cursorLine.setAttribute("x1", x(state.cursor));
        cursorLine.setAttribute("x2", x(state.cursor));
        var hovered = null;
        occupied(state.cursor).forEach(function (slice) {
          if (slice.band === state.band) { hovered = slice; }
        });
        if (hovered) {
          bandMark.setAttribute("cx", x(state.cursor));
          bandMark.setAttribute("cy", (y(hovered.from) + y(hovered.to)) / 2);
          bandMark.setAttribute("visibility", "visible");
        } else {
          bandMark.setAttribute("visibility", "hidden");
        }
      }

      function selectIndex(i, band) {
        state.cursor = Math.max(0, Math.min(series.length - 1, i));
        if (band !== undefined) { state.band = band; }
        moveCursor();
        drawComposition(data, series, state.cursor);
        drawTooltip(data, series, state.cursor);
      }

      // Arrow keys do exactly what the cursor does — left and right across
      // commits, up and down through the stack — so both halves of the tooltip
      // are reachable without a pointer.
      interaction.phases = function (event) {
        if (event.type === "keydown") {
          if (event.key === "ArrowRight") { selectIndex(state.cursor + 1); event.preventDefault(); }
          if (event.key === "ArrowLeft") { selectIndex(state.cursor - 1); event.preventDefault(); }
          if (event.key === "ArrowUp" || event.key === "ArrowDown") {
            var column = occupied(state.cursor);
            if (column.length) {
              // The stack is built from the axis upwards, so "up" is the next
              // band away from it.
              var step = event.key === "ArrowUp" ? 1 : -1;
              var at = -1;
              column.forEach(function (slice, k) { if (slice.band === state.band) { at = k; } });
              var next = at === -1
                ? (step === 1 ? 0 : column.length - 1)
                : Math.max(0, Math.min(column.length - 1, at + step));
              selectIndex(state.cursor, column[next].band);
            }
            event.preventDefault();
          }
          return;
        }
        var at = svgLocation(svg, event);
        if (!at) { return; }
        var i = Math.round(((at.x - 40) / 840) * (series.length - 1));
        i = Math.max(0, Math.min(series.length - 1, i));
        selectIndex(i, bandAt(i, at.y));
      };

      svg.setAttribute("aria-label", "Phase composition for " + state.workload +
        ". Use left and right arrow keys to move between measured commits, and up " +
        "and down to move through the phase bands.");

      caption.textContent = series.length +
        " measured commit(s). Bands sum exactly to compiler-root time.";

      var start = state.cursor === null
        ? series.length - 1
        : Math.min(state.cursor, series.length - 1);
      // Open on the largest band rather than on none, so the band line is
      // present before the first hover and the feature is visible at rest.
      var initial = state.band;
      if (initial === null) {
        var largest = null;
        occupied(start).forEach(function (slice) {
          if (!largest || slice.to - slice.from > largest.to - largest.from) { largest = slice; }
        });
        initial = largest ? largest.band : null;
      }
      selectIndex(start, initial);
    }

    // 4. Composition bar for the cursored commit; also the stack's legend.
    function drawComposition(data, series, index) {
      var container = document.getElementById("perf-composition");
      container.textContent = "";
      var point = series[index].point.workloads[state.workload];
      var total = point.compiler_root_ns || 1;

      var bar = document.createElement("div");
      bar.style.cssText = "display:flex;height:1.75rem;width:100%;border:1px solid #ccc;";
      var legend = document.createElement("ul");
      legend.style.cssText =
        "list-style:none;padding:0;margin:0.5rem 0 0;display:flex;flex-wrap:wrap;gap:0.75rem;font-size:0.85rem;";

      (data.bands || []).forEach(function (band) {
        var value = point.bands_ns[band] || 0;
        if (!value) { return; }
        var share = (value / total) * 100;
        var colour = BAND_COLOURS[band] || "#ccc";

        // The hovered band is named in the tooltip; marking it here as well is
        // what ties the two surfaces together, since this bar is also the
        // stack's legend.
        var hovered = band === state.band;

        var segment = document.createElement("div");
        segment.style.cssText = "width:" + share + "%;background:" + colour + ";" +
          (hovered ? "outline:2px solid #111;outline-offset:-2px;" : "");
        segment.title = band + " " + fmtMs(value);
        bar.appendChild(segment);

        var item = document.createElement("li");
        if (hovered) { item.style.fontWeight = "600"; }
        item.innerHTML = '<span style="display:inline-block;width:0.75rem;height:0.75rem;background:' +
          colour + '"></span> ' + escapeHtml(band) + " " + fmtMs(value) + " (" + share.toFixed(1) + "%)";
        legend.appendChild(item);
      });

      container.appendChild(bar);
      container.appendChild(legend);
    }

    // Tooltip content per §11: commit short hash and subject, the hovered band
    // and its value, the total, the delta against the previous measured commit,
    // and commits-since-last-measurement when runs were skipped. The flagging
    // state and the environment advisory are additions to that list, not part
    // of it.
    function drawTooltip(data, series, index) {
      var tooltip = document.getElementById("perf-tooltip");
      var entry = series[index];
      var point = entry.point.workloads[state.workload];

      var lines = [
        commitHeading(data, entry.point.commit),
        "Measured " + fmtWhen(entry.point.finished_at) + "."
      ];

      // The composition bar carries every band for the whole commit. This is
      // the one the cursor is actually on, which is what §11 asks the tooltip
      // for and what the bar alone cannot say.
      var total = point.compiler_root_ns || 1;
      if (state.band) {
        var value = point.bands_ns[state.band] || 0;
        lines.push("Band: " + escapeHtml(state.band) + " " + fmtMs(value) +
          " (" + ((value / total) * 100).toFixed(1) + "% of compiler root)");
      } else {
        lines.push("Band: none under the cursor, which is above the stack.");
      }

      lines.push("Total: " + fmtMs(point.compiler_root_ns) + " compiler root, " +
        fmtMs(point.process_elapsed_ns) + " process");

      if (index > 0) {
        var previous = series[index - 1];
        var delta = (point.median_ns / previous.point.workloads[state.workload].median_ns - 1) * 100;
        lines.push("Versus the previous measured commit (" +
          escapeHtml(shortHash(previous.point.commit)) + "): " +
          (delta >= 0 ? "+" : "") + delta.toFixed(2) + "%");
        var skipped = skippedLine(data, previous.point.commit, entry.point.commit);
        if (skipped) { lines.push(skipped); }
      } else {
        lines.push("First measured commit in this view.");
      }

      lines.push(flaggingLine(entry.epoch, point));

      var drift = entry.epoch.environment_annotations.filter(function (note) {
        return note.commit === entry.point.commit;
      });
      if (drift.length) {
        lines.push("Environment changed here, so comparisons across it are advisory: " +
          escapeHtml(drift[0].changed.join("; ")));
      }

      setTooltip(tooltip, lines);
    }

    // 6. Field notes.
    function drawNotes() {
      var list = document.getElementById("perf-notes");
      list.textContent = "";
      var any = false;
      current().epochs.forEach(function (epoch) {
        epoch.environment_annotations.forEach(function (note) {
          any = true;
          var item = document.createElement("li");
          item.textContent = shortHash(note.commit) + " — environment changed: " +
            note.changed.join("; ") + ". Comparisons across this point are advisory.";
          list.appendChild(item);
        });
        // Deliberately not marked advisory. The standard library is part of the
        // product being measured, so a change here is real movement in what the
        // series tracks — the note says where it happened, not that the
        // comparison is untrustworthy.
        (epoch.stdlib_annotations || []).forEach(function (note) {
          any = true;
          var item = document.createElement("li");
          item.textContent = shortHash(note.commit) +
            " — standard library changed. Measured, not excluded: std is part of " +
            "the product, so its cost moves the series like any compiler change.";
          list.appendChild(item);
        });
      });
      if (!any) {
        var item = document.createElement("li");
        item.style.color = "var(--color-muted)";
        item.textContent = "No annotations in the visible window.";
        list.appendChild(item);
      }
    }

    // 7. Disclosures.
    function drawDisclosures(data, points) {
      var latest = points[points.length - 1].point;

      var overhead = document.getElementById("perf-overhead");
      var others = document.getElementById("perf-other-metrics");
      overhead.textContent = "";
      others.textContent = "";

      Object.keys(latest.workloads).forEach(function (name) {
        var workload = latest.workloads[name];

        var row = document.createElement("div");
        row.textContent = name + ": " + fmtMs(workload.driver_overhead_ns) +
          " outside compiler root (" + fmtMs(workload.process_elapsed_ns) + " process, " +
          fmtMs(workload.compiler_root_ns) + " root)";
        overhead.appendChild(row);

        var other = document.createElement("div");
        other.textContent = name + ": " +
          (workload.peak_memory_bytes / 1048576).toFixed(1) + " MiB peak, " +
          workload.output_binary_bytes + " B binary";
        others.appendChild(other);
      });

      var rejected = document.getElementById("perf-rejected");
      rejected.textContent = "";
      if (!data.rejected || !data.rejected.length) {
        rejected.textContent = "No runs were rejected.";
        return;
      }
      data.rejected.forEach(function (run) {
        var row = document.createElement("div");
        row.textContent = run.platform + " epoch " + run.epoch + " " +
          shortHash(run.commit) + ": " + run.reasons.join("; ");
        rejected.appendChild(row);
      });
    }

    draw();
  }
})();
