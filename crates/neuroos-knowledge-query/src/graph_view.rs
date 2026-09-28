//! FR-KNO-11 / P5-S08: renders a self-contained `graph_view.html` --
//! inline D3 (vendored at `ui/graph-view/vendor/d3.v7.min.js`, embedded at
//! compile time), inline CSS, inline JSON data, **zero network requests**
//! (design.md §6.2's own requirement -- this module's own tests assert the
//! rendered page contains no `http://`/`https://` substring at all, so a
//! future edit can't accidentally reintroduce a CDN reference).
use neuroos_proto::v1::{EdgeRow, EntityRow};
use serde::Serialize;

/// AB-1 forbids C5a linking `neuroos-storage`, so this can't import its
/// `adapters::family_for_domain` -- duplicated here instead (small, stable,
/// Architecture.md §7.3's own domain/family table, not derived data).
fn family_for_domain(domain: &str) -> &'static str {
    match domain {
        "window_focus" | "app_lifecycle" | "idle_presence" => "attention",
        "process_activity" | "build_job" | "git_activity" => "work",
        "notes" | "calendar" => "knowledge",
        "media_playback" | "system_resource" => "system",
        _ => "external",
    }
}

const D3_JS: &str = include_str!("../../../ui/graph-view/vendor/d3.v7.min.js");

#[derive(Debug, Serialize)]
struct GraphNode {
    id: i64,
    domain: String,
    family: &'static str,
    kind: String,
    label: String,
    tainted: bool,
    last_seen_ns: u64,
}

#[derive(Debug, Serialize)]
struct GraphEdge {
    source: i64,
    target: i64,
    kind: String,
    weight: f32,
    hypothesis: bool,
}

#[derive(Debug, Serialize)]
struct GraphData {
    generated_at_ns: u64,
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

/// Renders the full page. `entities`/`edges` come from `storage.sock`
/// (`StorageClient::list_entities`/`list_edges`) -- this function itself is
/// pure and IPC-free, so it's unit-testable without a running C3.
pub fn render(entities: &[EntityRow], edges: &[EdgeRow], generated_at_ns: u64) -> String {
    let data = GraphData {
        generated_at_ns,
        nodes: entities
            .iter()
            .map(|e| GraphNode {
                id: e.id,
                family: family_for_domain(&e.domain),
                domain: e.domain.clone(),
                kind: e.kind.clone(),
                label: e.label.clone(),
                tainted: e.taint != 0,
                last_seen_ns: e.last_seen_ns,
            })
            .collect(),
        edges: edges
            .iter()
            .map(|e| GraphEdge {
                source: e.src,
                target: e.dst,
                kind: e.kind.clone(),
                weight: e.weight,
                hypothesis: e.hypothesis,
            })
            .collect(),
    };
    // `to_string` over a plain struct of numbers/strings/bools never fails;
    // the empty-graph fallback only matters if that invariant is ever
    // broken by a future field type.
    let data_json =
        serde_json::to_string(&data).unwrap_or_else(|_| r#"{"nodes":[],"edges":[]}"#.to_string());

    PAGE_TEMPLATE
        .replace("__D3_JS__", D3_JS)
        .replace("__DATA_JSON__", &data_json)
        .replace("__NODE_COUNT__", &data.nodes.len().to_string())
        .replace("__EDGE_COUNT__", &data.edges.len().to_string())
        .replace("__GENERATED_AT_NS__", &generated_at_ns.to_string())
}

/// design.md §6: header (search + family filter + theme toggle), legend +
/// stats sidebar, force-directed canvas, detail card, footer. Colors follow
/// design.md §5's dark/light token pairs; family palette picked to be
/// distinguishable and consistent with the legend order in §6.1's mockup
/// (Attention, Work, Knowledge, System, External).
const PAGE_TEMPLATE: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>NeuroOS &middot; Knowledge graph</title>
<style>
:root {
  --bg-surface: #ffffff; --bg-canvas: #f6f7f9; --text-primary: #1f2328; --text-muted: #6b7280;
  --border: #d0d7de; --accent: #2f6fed;
  --family-attention: #2f6fed; --family-work: #16a34a; --family-knowledge: #a855f7;
  --family-system: #f59e0b; --family-external: #e11d48; --hypothesis: #9ca3af;
}
@media (prefers-color-scheme: dark) {
  :root { --bg-surface: #15171b; --bg-canvas: #0d0f12; --text-primary: #e6e8eb; --text-muted: #9aa1ac; --border: #2b2f36; }
}
:root[data-theme="light"] { --bg-surface: #ffffff; --bg-canvas: #f6f7f9; --text-primary: #1f2328; --text-muted: #6b7280; --border: #d0d7de; }
:root[data-theme="dark"] { --bg-surface: #15171b; --bg-canvas: #0d0f12; --text-primary: #e6e8eb; --text-muted: #9aa1ac; --border: #2b2f36; }
* { box-sizing: border-box; }
body { margin: 0; font-family: -apple-system, "Segoe UI", Roboto, sans-serif; color: var(--text-primary); background: var(--bg-canvas); }
header { height: 56px; display: flex; align-items: center; gap: 16px; padding: 0 16px; background: var(--bg-surface); border-bottom: 1px solid var(--border); }
header h1 { font-size: 15px; font-weight: 600; margin: 0; white-space: nowrap; }
header input, header select, header button { font: inherit; padding: 6px 10px; border-radius: 8px; border: 1px solid var(--border); background: var(--bg-canvas); color: var(--text-primary); }
header input { flex: 0 1 260px; }
main { display: grid; grid-template-columns: 220px 1fr 300px; height: calc(100vh - 56px - 32px); }
aside.legend { padding: 16px; border-right: 1px solid var(--border); background: var(--bg-surface); overflow-y: auto; }
aside.legend h2 { font-size: 11px; text-transform: uppercase; color: var(--text-muted); margin: 0 0 8px; }
.legend-item { display: flex; align-items: center; gap: 8px; font-size: 13px; margin-bottom: 6px; }
.dot { width: 10px; height: 10px; border-radius: 50%; display: inline-block; flex: none; }
.stats-row { display: flex; justify-content: space-between; font-size: 13px; margin-bottom: 4px; }
#canvas-wrap { position: relative; overflow: hidden; }
svg { width: 100%; height: 100%; display: block; }
.node-label { font-size: 10px; fill: var(--text-primary); pointer-events: none; paint-order: stroke; stroke: var(--bg-canvas); stroke-width: 3px; }
#detail { padding: 16px; border-left: 1px solid var(--border); background: var(--bg-surface); overflow-y: auto; }
#detail h2 { font-size: 11px; text-transform: uppercase; color: var(--text-muted); margin: 0 0 8px; }
#detail .card { border: 1px solid var(--border); border-radius: 8px; padding: 12px; font-size: 13px; }
#detail .card .label { font-weight: 600; margin-bottom: 4px; word-break: break-word; }
#detail .card .meta { color: var(--text-muted); font-size: 12px; margin-top: 4px; }
.tainted-badge { display: inline-block; margin-top: 8px; font-size: 11px; font-weight: 600; color: var(--family-external); border: 1px solid var(--family-external); border-radius: 999px; padding: 2px 8px; }
footer { height: 32px; display: flex; align-items: center; justify-content: center; font-size: 12px; color: var(--text-muted); background: var(--bg-surface); border-top: 1px solid var(--border); }
#sr-only-table { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0 0 0 0); }
</style>
</head>
<body>
<header>
  <h1>NeuroOS &middot; Knowledge graph</h1>
  <input id="search" type="search" placeholder="Search nodes&hellip; (press /)" aria-label="Search nodes">
  <select id="family-filter" aria-label="Filter by family">
    <option value="">All families</option>
    <option value="attention">Attention</option>
    <option value="work">Work</option>
    <option value="knowledge">Knowledge</option>
    <option value="system">System</option>
    <option value="external">External</option>
  </select>
  <button id="theme-toggle" type="button" aria-label="Toggle theme">&#9680; Theme</button>
</header>
<main>
  <aside class="legend">
    <h2>Legend</h2>
    <div class="legend-item"><span class="dot" style="background:var(--family-attention)"></span>Attention</div>
    <div class="legend-item"><span class="dot" style="background:var(--family-work)"></span>Work</div>
    <div class="legend-item"><span class="dot" style="background:var(--family-knowledge)"></span>Knowledge</div>
    <div class="legend-item"><span class="dot" style="background:var(--family-system)"></span>System</div>
    <div class="legend-item"><span class="dot" style="background:var(--family-external)"></span>External</div>
    <div class="legend-item">&mdash; &mdash; hypothesis edge</div>
    <h2 style="margin-top:20px">Stats</h2>
    <div class="stats-row"><span>Nodes</span><strong id="stat-nodes">__NODE_COUNT__</strong></div>
    <div class="stats-row"><span>Edges</span><strong id="stat-edges">__EDGE_COUNT__</strong></div>
    <div class="stats-row"><span>Generated</span><strong id="stat-generated"></strong></div>
  </aside>
  <div id="canvas-wrap"><svg id="graph" role="img" aria-label="Knowledge graph force-directed layout"></svg></div>
  <section id="detail" aria-live="polite">
    <h2>Details</h2>
    <div id="detail-body">Select a node to see details.</div>
  </section>
</main>
<footer id="footer-text">Generated locally &middot; no network requests</footer>
<table id="sr-only-table"><caption>Knowledge graph nodes (screen-reader accessible list)</caption>
  <thead><tr><th>Label</th><th>Domain</th><th>Family</th><th>Tainted</th></tr></thead>
  <tbody id="sr-only-tbody"></tbody>
</table>
<script>__D3_JS__</script>
<script>
(function () {
  "use strict";
  var DATA = __DATA_JSON__;
  var GENERATED_AT_NS = __GENERATED_AT_NS__;

  var generatedDate = new Date(GENERATED_AT_NS / 1e6);
  document.getElementById("stat-generated").textContent = generatedDate.toISOString();
  document.getElementById("footer-text").textContent =
    "Generated locally · no network requests · snapshot of " + generatedDate.toISOString();

  var srBody = document.getElementById("sr-only-tbody");
  DATA.nodes.forEach(function (n) {
    var tr = document.createElement("tr");
    tr.innerHTML = "<td>" + n.label + "</td><td>" + n.domain + "</td><td>" + n.family + "</td><td>" + (n.tainted ? "yes" : "no") + "</td>";
    srBody.appendChild(tr);
  });

  var svg = d3.select("#graph");
  var wrap = document.getElementById("canvas-wrap");
  var width = wrap.clientWidth || 800;
  var height = wrap.clientHeight || 600;

  var zoomLayer = svg.append("g");
  svg.call(d3.zoom().scaleExtent([0.2, 8]).on("zoom", function (event) {
    zoomLayer.attr("transform", event.transform);
  }));

  var degree = {};
  DATA.nodes.forEach(function (n) { degree[n.id] = 0; });
  DATA.edges.forEach(function (e) {
    degree[e.source] = (degree[e.source] || 0) + 1;
    degree[e.target] = (degree[e.target] || 0) + 1;
  });
  var radiusScale = d3.scaleSqrt().domain([0, d3.max(Object.values(degree)) || 1]).range([4, 14]);

  var familyColor = function (family) {
    return getComputedStyle(document.documentElement).getPropertyValue("--family-" + family).trim() || "#888";
  };

  var linkSel = zoomLayer.append("g").attr("stroke-opacity", 0.5)
    .selectAll("line").data(DATA.edges).join("line")
    .attr("stroke", function (d) { return d.hypothesis ? "var(--hypothesis)" : "var(--text-muted)"; })
    .attr("stroke-dasharray", function (d) { return d.hypothesis ? "4,3" : null; })
    .attr("stroke-width", function (d) { return Math.max(1, Math.min(4, d.weight)); });

  var nodeSel = zoomLayer.append("g")
    .selectAll("g").data(DATA.nodes).join("g")
    .attr("tabindex", 0)
    .attr("role", "button")
    .attr("aria-label", function (d) { return d.label + " (" + d.family + ")"; })
    .style("cursor", "pointer");

  nodeSel.append("path")
    .attr("d", function (d) {
      var r = radiusScale(degree[d.id] || 0);
      if (d.tainted) {
        // diamond shape for tainted entities (design.md §6.2).
        return "M0," + -r + " L" + r + ",0 L0," + r + " L" + -r + ",0 Z";
      }
      return d3.symbol().type(d3.symbolCircle).size(Math.PI * r * r)();
    })
    .attr("fill", function (d) { return familyColor(d.family); })
    .attr("stroke", "var(--bg-canvas)")
    .attr("stroke-width", 1.5);

  var labelSel = zoomLayer.append("g")
    .selectAll("text").data(DATA.nodes).join("text")
    .attr("class", "node-label")
    .attr("dy", -12)
    .attr("text-anchor", "middle")
    .style("display", "none")
    .text(function (d) { return d.label; });

  var simulation = d3.forceSimulation(DATA.nodes)
    .force("link", d3.forceLink(DATA.edges).id(function (d) { return d.id; }).distance(60).strength(0.3))
    .force("charge", d3.forceManyBody().strength(-120))
    .force("center", d3.forceCenter(width / 2, height / 2))
    .force("collide", d3.forceCollide(function (d) { return radiusScale(degree[d.id] || 0) + 4; }));

  simulation.on("tick", function () {
    linkSel
      .attr("x1", function (d) { return d.source.x; }).attr("y1", function (d) { return d.source.y; })
      .attr("x2", function (d) { return d.target.x; }).attr("y2", function (d) { return d.target.y; });
    nodeSel.attr("transform", function (d) { return "translate(" + d.x + "," + d.y + ")"; });
    labelSel.attr("x", function (d) { return d.x; }).attr("y", function (d) { return d.y; });
  });

  var selectedId = null;

  function renderDetail(d) {
    var body = document.getElementById("detail-body");
    if (!d) { body.textContent = "Select a node to see details."; return; }
    var seen = new Date(d.last_seen_ns / 1e6).toISOString();
    body.innerHTML =
      '<div class="card"><div class="label"></div><div class="meta"></div>' +
      (d.tainted ? '<span class="tainted-badge">FROM THE WEB</span>' : "") + "</div>";
    body.querySelector(".label").textContent = d.label;
    body.querySelector(".meta").textContent = d.domain + " · " + d.kind + " · last seen " + seen;
  }

  function applyHighlight() {
    var neighbours = null;
    if (selectedId !== null) {
      neighbours = new Set([selectedId]);
      DATA.edges.forEach(function (e) {
        var s = e.source.id !== undefined ? e.source.id : e.source;
        var t = e.target.id !== undefined ? e.target.id : e.target;
        if (s === selectedId) neighbours.add(t);
        if (t === selectedId) neighbours.add(s);
      });
    }
    nodeSel.style("opacity", function (d) {
      return neighbours === null || neighbours.has(d.id) ? 1 : 0.25;
    });
  }

  nodeSel.on("click", function (event, d) {
    selectedId = d.id;
    renderDetail(d);
    applyHighlight();
  });

  var zoomBehaviour = d3.zoom().scaleExtent([0.2, 8]).on("zoom", function (event) {
    zoomLayer.attr("transform", event.transform);
    labelSel.style("display", event.transform.k >= 1.5 ? null : "none");
  });
  svg.call(zoomBehaviour);

  var searchBox = document.getElementById("search");
  function applySearch() {
    var q = searchBox.value.trim().toLowerCase();
    nodeSel.style("display", function (d) {
      return q === "" || d.label.toLowerCase().indexOf(q) !== -1 ? null : "none";
    });
    labelSel.style("visibility", function (d) {
      return q === "" || d.label.toLowerCase().indexOf(q) !== -1 ? "visible" : "hidden";
    });
  }
  searchBox.addEventListener("input", applySearch);

  var familyFilter = document.getElementById("family-filter");
  familyFilter.addEventListener("change", function () {
    var f = familyFilter.value;
    nodeSel.style("opacity", function (d) { return f === "" || d.family === f ? 1 : 0.1; });
  });

  document.getElementById("theme-toggle").addEventListener("click", function () {
    var root = document.documentElement;
    var current = root.getAttribute("data-theme");
    root.setAttribute("data-theme", current === "dark" ? "light" : "dark");
  });

  document.addEventListener("keydown", function (event) {
    if (event.key === "/") { event.preventDefault(); searchBox.focus(); }
    else if (event.key === "Escape") { selectedId = null; renderDetail(null); applyHighlight(); searchBox.blur(); }
    else if (event.key === "+" || event.key === "=") { svg.transition().call(zoomBehaviour.scaleBy, 1.3); }
    else if (event.key === "-") { svg.transition().call(zoomBehaviour.scaleBy, 1 / 1.3); }
  });
})();
</script>
</body>
</html>
"##;

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)] // rules.md §5 scoped to non-test code
    use super::*;

    fn entity(id: i64, domain: &str, label: &str, taint: u32) -> EntityRow {
        EntityRow {
            id,
            domain: domain.to_string(),
            kind: "window".to_string(),
            label: label.to_string(),
            taint,
            created_ns: 0,
            last_seen_ns: 0,
            permanent: false,
        }
    }

    fn edge(src: i64, dst: i64, hypothesis: bool) -> EdgeRow {
        EdgeRow {
            src,
            dst,
            kind: "co_occurs".to_string(),
            weight: 1.0,
            reinforced_ns: 0,
            hypothesis,
        }
    }

    #[test]
    fn family_for_domain_covers_every_domain_adapter() {
        assert_eq!(family_for_domain("window_focus"), "attention");
        assert_eq!(family_for_domain("app_lifecycle"), "attention");
        assert_eq!(family_for_domain("idle_presence"), "attention");
        assert_eq!(family_for_domain("process_activity"), "work");
        assert_eq!(family_for_domain("build_job"), "work");
        assert_eq!(family_for_domain("git_activity"), "work");
        assert_eq!(family_for_domain("notes"), "knowledge");
        assert_eq!(family_for_domain("calendar"), "knowledge");
        assert_eq!(family_for_domain("media_playback"), "system");
        assert_eq!(family_for_domain("system_resource"), "system");
        assert_eq!(family_for_domain("external_documents"), "external");
        assert_eq!(family_for_domain("voice_interaction"), "external");
    }

    #[test]
    fn render_makes_zero_network_requests() {
        // design.md §6.2: "The page must make zero network requests" --
        // no markup construct that makes the browser *automatically* fetch
        // something just by parsing the page (an external `<script src>`,
        // `<link>`, `<img>`, `<iframe>`, or CSS `@import`). A plain-text URL
        // (D3's own vendored comment header) or an unused library function
        // that merely *accepts* a URL argument (D3 bundles `d3.json`/
        // `d3.csv`, which this page's own script never calls) never issues
        // a request on its own, so neither is checked here.
        let html = render(&[entity(1, "window_focus", "firefox", 0)], &[], 0);
        assert!(!html.contains("<script src"));
        assert!(!html.contains("<link "));
        assert!(!html.contains("<img "));
        assert!(!html.contains("<iframe"));
        assert!(!html.contains("@import"));
    }

    #[test]
    fn render_embeds_every_entity_and_edge() {
        let entities = vec![
            entity(1, "window_focus", "firefox", 0),
            entity(2, "external_documents", "doc-42", 1),
        ];
        let edges = vec![edge(1, 2, true)];
        let html = render(&entities, &edges, 1_700_000_000_000_000_000);

        assert!(html.contains("\"id\":1"));
        assert!(html.contains("firefox"));
        assert!(html.contains("\"tainted\":true"));
        assert!(html.contains("\"source\":1"));
        assert!(html.contains("\"target\":2"));
        assert!(html.contains("\"hypothesis\":true"));
    }

    #[test]
    fn render_reports_accurate_node_and_edge_counts() {
        let entities = vec![entity(1, "notes", "a", 0), entity(2, "notes", "b", 0)];
        let edges = vec![edge(1, 2, false)];
        let html = render(&entities, &edges, 0);
        assert!(html.contains(">2</strong>")); // node count
        assert!(html.contains(">1</strong>")); // edge count
    }

    #[test]
    fn render_of_an_empty_graph_still_produces_valid_looking_html() {
        let html = render(&[], &[], 0);
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.trim_end().ends_with("</html>"));
        assert!(html.contains("\"nodes\":[]"));
        assert!(html.contains("\"edges\":[]"));
    }

    #[test]
    fn render_includes_the_vendored_d3_bundle_inline() {
        let html = render(&[], &[], 0);
        assert!(html.contains("d3js.org"));
    }

    #[test]
    fn render_escapes_nothing_it_does_not_need_to_and_stays_valid_json() {
        // A label with characters that matter to both HTML and JSON must
        // round-trip through `serde_json` without breaking the embedding
        // (the label is only ever placed inside a JSON string literal,
        // never directly into HTML markup).
        let html = render(&[entity(1, "notes", "a \"quoted\" <tag> label", 0)], &[], 0);
        let start = html.find("var DATA = ").unwrap() + "var DATA = ".len();
        let end = html[start..].find(";\n").unwrap() + start;
        let parsed: serde_json::Value = serde_json::from_str(&html[start..end]).unwrap();
        assert_eq!(parsed["nodes"][0]["label"], "a \"quoted\" <tag> label");
    }
}

#[cfg(test)]
mod manual_inspection {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    #[ignore = "manual: writes a sample graph_view.html to /tmp for visual/browser inspection"]
    fn dump_sample_html() {
        let entities = vec![
            EntityRow {
                id: 1,
                domain: "window_focus".into(),
                kind: "window".into(),
                label: "quarterly revenue dashboard".into(),
                taint: 0,
                created_ns: 0,
                last_seen_ns: 1_700_000_000_000_000_000,
                permanent: false,
            },
            EntityRow {
                id: 2,
                domain: "git_activity".into(),
                kind: "repo".into(),
                label: "neuroos".into(),
                taint: 0,
                created_ns: 0,
                last_seen_ns: 1_700_000_000_000_000_000,
                permanent: false,
            },
            EntityRow {
                id: 3,
                domain: "external_documents".into(),
                kind: "document".into(),
                label: "api-docs.html".into(),
                taint: 1,
                created_ns: 0,
                last_seen_ns: 1_700_000_000_000_000_000,
                permanent: true,
            },
            EntityRow {
                id: 4,
                domain: "notes".into(),
                kind: "note".into(),
                label: "sprint plan".into(),
                taint: 0,
                created_ns: 0,
                last_seen_ns: 1_700_000_000_000_000_000,
                permanent: true,
            },
        ];
        let edges = vec![
            EdgeRow {
                src: 1,
                dst: 2,
                kind: "co_occurs".into(),
                weight: 2.0,
                reinforced_ns: 0,
                hypothesis: false,
            },
            EdgeRow {
                src: 1,
                dst: 3,
                kind: "co_occurs".into(),
                weight: 1.0,
                reinforced_ns: 0,
                hypothesis: true,
            },
            EdgeRow {
                src: 2,
                dst: 4,
                kind: "co_occurs".into(),
                weight: 1.5,
                reinforced_ns: 0,
                hypothesis: true,
            },
        ];
        let html = render(&entities, &edges, 1_700_000_000_000_000_000);
        std::fs::write("/tmp/graph_view_sample.html", html).unwrap();
    }
}
