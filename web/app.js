"use strict";

const $ = (id) => document.getElementById(id);
const REFRESH_MS = 1000;
let paused = false;
let failures = 0;

function clock(t) {
  if (t < 120) return Math.round(t) + "s";
  const total = Math.round(t);
  return Math.floor(total / 60) + ":" + String(total % 60).padStart(2, "0");
}

function num(v, digits) {
  if (v === null || v === undefined) return "–";
  return Number(v).toLocaleString(undefined, { maximumFractionDigits: digits });
}

function setStatus(text, kind) {
  const el = $("status");
  el.textContent = text;
  el.className = "pill pill-" + kind;
}

function renderTiles(latest) {
  if (!latest) return;
  $("v-waiting").textContent = num(latest.waiting, 0);
  $("v-running").textContent = num(latest.running, 0);
  $("v-tokens").textContent = latest.token_rate === null ? "–" : num(latest.token_rate, 0);
  $("v-kv").textContent = latest.kv_usage === null ? "–" : Math.round(latest.kv_usage * 100) + "%";
}

function renderRules(rules) {
  const box = $("rules");
  box.replaceChildren();
  for (const r of rules) {
    const card = document.createElement("div");
    card.className = "rule";

    const head = document.createElement("div");
    head.className = "rule-head";
    const dot = document.createElement("span");
    dot.className = "dot " + (r.state === "firing" ? "firing " + r.severity : r.state === "ok" ? "ok" : "");
    const unavailable = r.state === "unavailable";
    const name = document.createElement("span");
    name.className = "rule-name";
    name.textContent = r.name;
    const state = document.createElement("span");
    state.className = "rule-state";
    state.textContent = r.state === "firing" ? "firing" : r.state === "ok" ? "ok" : unavailable ? "unavailable" : "not enough data yet";
    head.append(dot, name, state);

    const desc = document.createElement("p");
    desc.textContent = r.description;
    const pattern = document.createElement("code");
    pattern.className = "pattern";
    pattern.textContent = r.pattern;
    const meta = document.createElement("p");
    meta.className = "meta";
    meta.textContent = r.severity + " · looks back " + r.lookback_s + "s";

    card.append(head, desc, pattern, meta);
    if (unavailable) {
      const note = document.createElement("p");
      note.className = "note";
      note.textContent = missingText(r.missing);
      card.append(note);
    }
    box.append(card);
  }
}

const SIGNAL_WORDS = {
  kv_usage: "KV cache usage",
  preemption_rate: "preemption counts",
  token_rate: "the generated-token counter",
  progress_rate: "the token counters",
};

function missingText(missing) {
  const words = missing.map((m) => SIGNAL_WORDS[m] || m);
  return "Disabled: this server does not export " + words.join(" or ") + ".";
}

function describeEvent(e) {
  switch (e.event) {
    case "violation": return { tag: e.severity, label: e.severity, text: e.rule + ": " + e.description };
    case "recovered": return { tag: "recovered", label: "recovered", text: e.rule };
    case "scrape_failing": return { tag: "critical", label: "no data", text: "metrics endpoint unreachable (" + e.consecutive + " tries): " + e.error };
    case "scrape_recovered": return { tag: "recovered", label: "recovered", text: "metrics endpoint is back" };
    default: return { tag: "info", label: e.event, text: "" };
  }
}

function renderEvents(events) {
  const list = $("events");
  list.replaceChildren();
  $("no-events").hidden = events.length > 0;
  for (const e of events) {
    const d = describeEvent(e);
    const li = document.createElement("li");
    const when = document.createElement("span");
    when.className = "when";
    when.textContent = clock(e.t);
    const tag = document.createElement("span");
    tag.className = "tag " + d.tag;
    tag.textContent = d.label;
    const text = document.createElement("span");
    text.textContent = d.text;
    li.append(when, tag, text);
    list.append(li);
  }
}

function renderHeader(state) {
  $("mode").textContent = state.server ? state.mode + " · " + state.server : state.mode;
  const firing = state.rules.filter((r) => r.state === "firing");
  if (state.scrape_error) setStatus("metrics unreachable", "crit");
  else if (!state.latest) setStatus("waiting for data", "wait");
  else if (firing.some((r) => r.severity === "critical")) setStatus(firing.length + " firing", "crit");
  else if (firing.length) setStatus(firing.length + " warning", "warn");
  else setStatus("all clear", "ok");

  const banner = $("banner");
  if (state.scrape_error) {
    banner.hidden = false;
    banner.className = "banner error";
    banner.textContent = "Last scrape failed: " + state.scrape_error;
  } else if (state.mode.startsWith("demo")) {
    banner.hidden = false;
    banner.className = "banner";
    banner.textContent = "Demo mode. These numbers are synthetic and do not come from a real server.";
  } else {
    banner.hidden = true;
  }
}

async function refresh() {
  if (paused) return;
  try {
    const last = $("window").value;
    const [stateRes, svgRes] = await Promise.all([
      fetch("/api/state", { cache: "no-store" }),
      fetch("/timeline.svg?last=" + encodeURIComponent(last), { cache: "no-store" }),
    ]);
    if (!stateRes.ok || !svgRes.ok) throw new Error("HTTP " + stateRes.status + "/" + svgRes.status);
    const state = await stateRes.json();
    $("timeline").innerHTML = await svgRes.text();
    failures = 0;
    renderHeader(state);
    renderTiles(state.latest);
    renderRules(state.rules);
    renderEvents(state.events);
  } catch (err) {
    failures += 1;
    if (failures >= 2) {
      setStatus("UI cannot reach nexusguard", "crit");
      const banner = $("banner");
      banner.hidden = false;
      banner.className = "banner error";
      banner.textContent = "Lost connection to the nexusguard process: " + err.message;
    }
  }
}

$("pause").addEventListener("click", () => {
  paused = !paused;
  $("pause").textContent = paused ? "Resume" : "Pause";
  if (!paused) refresh();
});
$("window").addEventListener("change", refresh);

refresh();
setInterval(refresh, REFRESH_MS);
