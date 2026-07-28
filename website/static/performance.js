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

    var state = { platform: 0, workload: null, cursor: null };

    select.addEventListener("change", function () {
      state.platform = Number(select.value);
      state.workload = null;
      state.cursor = null;
      draw();
    });

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
      drawIndex(points);
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
      parts.push(flagged.length ? "Flagged: " + flagged.join(", ") + "." : "Nothing flagged.");
      status.textContent = parts.join(" ");

      if (latest.point.complete) {
        health.hidden = true;
      } else {
        health.hidden = false;
        health.textContent =
          "Collection health: the latest run was incomplete. Did not complete validly: " +
          latest.point.missing.join(", ") +
          ". No headline point is published from a partial run.";
      }
    }

    // 2. Headline chart. One polyline per epoch, so a baseline change reads as
    // a labelled boundary rather than as a change in the compiler.
    function drawIndex(points) {
      var svg = document.getElementById("perf-index");
      var caption = document.getElementById("perf-index-caption");
      svg.textContent = "";

      var indexed = points.filter(function (entry) { return entry.point.index; });
      if (!indexed.length) {
        caption.textContent = "No index points yet. A ratio needs a baseline to be a ratio of.";
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

      caption.textContent = indexed.length + " index point(s). Dashed orange marks an epoch " +
        "boundary; dotted blue marks an environment change, across which comparisons are advisory.";
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
        var flag = currentPoint.flagged === true
          ? " · flagged"
          : (currentPoint.flagged === null ? " · not enough history" : "");

        card.innerHTML =
          '<div style="font-weight:600;">' + escapeHtml(workload.id) + flag + "</div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">' + escapeHtml(workload.question) + "</div>" +
          '<div style="margin-top:0.4rem;">' + fmtMs(currentPoint.median_ns) +
          ' <span style="color:var(--color-muted);">± ' + fmtMs(currentPoint.mad_ns) + " MAD</span></div>" +
          '<div style="color:var(--color-muted);font-size:0.85rem;">Δ ' + delta + "</div>";
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
        return;
      }

      var hi = Math.max.apply(null, series.map(function (entry) {
        return entry.point.workloads[state.workload].compiler_root_ns;
      })) * 1.1;
      var x = function (i) { return 40 + (i / Math.max(series.length - 1, 1)) * 840; };
      var y = function (v) { return 230 - (v / hi) * 200; };

      var offsets = series.map(function () { return 0; });
      (data.bands || []).forEach(function (band) {
        var upper = [];
        var lower = [];
        series.forEach(function (entry, i) {
          lower.push(x(i) + "," + y(offsets[i]));
          offsets[i] += entry.point.workloads[state.workload].bands_ns[band] || 0;
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

      function selectIndex(i) {
        state.cursor = Math.max(0, Math.min(series.length - 1, i));
        drawComposition(data, series, state.cursor);
        drawTooltip(data, series, state.cursor);
      }

      svg.addEventListener("mousemove", function (event) {
        var box = svg.getBoundingClientRect();
        selectIndex(Math.round(((event.clientX - box.left) / box.width) * (series.length - 1)));
      });
      // Arrow keys do exactly what the cursor does, so the interaction is not
      // mouse-only and the tooltip's content is reachable without one.
      svg.addEventListener("keydown", function (event) {
        if (event.key === "ArrowRight") { selectIndex(state.cursor + 1); event.preventDefault(); }
        if (event.key === "ArrowLeft") { selectIndex(state.cursor - 1); event.preventDefault(); }
      });
      svg.setAttribute("aria-label", "Phase composition for " + state.workload +
        ". Use left and right arrow keys to move between measured commits.");

      caption.textContent = series.length +
        " measured commit(s). Bands sum exactly to compiler-root time.";
      selectIndex(state.cursor === null ? series.length - 1 : Math.min(state.cursor, series.length - 1));
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

        var segment = document.createElement("div");
        segment.style.cssText = "width:" + share + "%;background:" + colour + ";";
        segment.title = band + " " + fmtMs(value);
        bar.appendChild(segment);

        var item = document.createElement("li");
        item.innerHTML = '<span style="display:inline-block;width:0.75rem;height:0.75rem;background:' +
          colour + '"></span> ' + escapeHtml(band) + " " + fmtMs(value) + " (" + share.toFixed(1) + "%)";
        legend.appendChild(item);
      });

      container.appendChild(bar);
      container.appendChild(legend);
    }

    // Tooltip content per §11: commit and subject, the total, the delta against
    // the previous measured commit, and whether the comparison is advisory.
    function drawTooltip(data, series, index) {
      var tooltip = document.getElementById("perf-tooltip");
      var entry = series[index];
      var point = entry.point.workloads[state.workload];
      var subject = (data.commit_subjects || {})[entry.point.commit];

      var lines = [
        "<strong>" + escapeHtml(shortHash(entry.point.commit)) + "</strong>" +
          (subject ? " — " + escapeHtml(subject) : " (subject unavailable)"),
        "Total: " + fmtMs(point.compiler_root_ns) + " compiler root, " +
          fmtMs(point.process_elapsed_ns) + " process"
      ];

      if (index > 0) {
        var previous = series[index - 1];
        var delta = (point.median_ns / previous.point.workloads[state.workload].median_ns - 1) * 100;
        lines.push("Versus the previous measured commit (" +
          escapeHtml(shortHash(previous.point.commit)) + "): " +
          (delta >= 0 ? "+" : "") + delta.toFixed(2) + "%");
      } else {
        lines.push("First measured commit in this view.");
      }

      if (point.flagged === null) {
        lines.push("Movement unknown: the trailing window is not full yet.");
      } else if (point.flagged) {
        lines.push("Flagged: this exceeds the epoch's flagging rule.");
      }

      var drift = entry.epoch.environment_annotations.filter(function (note) {
        return note.commit === entry.point.commit;
      });
      if (drift.length) {
        lines.push("Environment changed here, so comparisons across it are advisory: " +
          escapeHtml(drift[0].changed.join("; ")));
      }

      tooltip.innerHTML = lines.join("<br>");
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
