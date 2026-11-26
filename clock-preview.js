"use strict";

const accounts = [
  { provider: "Claude", alias: "개인", used: { week: 36, short: 22, fable: 59 }, weekReset: 4 * 1440 + 360, shortReset: 135 },
  { provider: "GPT", alias: "Plus", used: { week: 70, short: 14 }, weekReset: 1440 + 180, shortReset: 45 },
  { provider: "GPT", alias: "Pro 200", used: { week: 8, short: 62 }, weekReset: 6 * 1440 + 720, shortReset: 255 },
];
const rings = {
  week: { name: "주간", color: "#b9a5ff", minutes: 7 * 1440 },
  fable: { name: "Fable 주간" },
  short: { name: "5시간", color: "#65ddd1", minutes: 300 },
};
const base = new Date(2026, 10, 26, 10, 24);
const $ = (selector) => document.querySelector(selector);
let mode = "hands", selected = 0, elapsed = 0;
let startedAt = Date.now(), advance = 0;

function duration(minutes) {
  if (minutes <= 0) return "갱신 대기";
  const days = Math.floor(minutes / 1440), hours = Math.floor(minutes % 1440 / 60);
  return [days && days + "일", hours && hours + "시간", minutes % 60 && minutes % 60 + "분"].filter(Boolean).join(" ");
}
function timeRatio(a, key) {
  const reset = resetTime(a, key);
  return Number.isFinite(reset) ? Math.max(0, Math.min(1, (reset - elapsed) / rings[key].minutes)) : undefined;
}
function bandKeys(a) {
  return Object.keys(dialBands).filter(key => Number.isFinite(a.used[key]));
}
function resetTime(a, key) {
  if (key === "short") return a.shortReset;
  return a.weekReset ?? a.fableReset;
}
function readoutTime(a, key) {
  const reset = key === "fable" ? (a.fableReset ?? a.weekReset) : a[key + "Reset"];
  return Number.isFinite(reset) ? duration(reset - elapsed) : "—";
}
function timeScale(a) {
  const rows = ["week", "short"].map((key, row) => {
    const { color } = rings[key], cells = dialBands[key];
    const width = (37 - (cells - 1)) / cells;
    return Array.from({ length: cells }, (_, i) => {
      const fill = width * Math.min(1, Math.max(0, (timeRatio(a, key) ?? 0) * cells - i));
      const x = i * (width + 1), y = row * 5.4;
      return '<rect x="' + x + '" y="' + y + '" width="' + width + '" height="3.6" rx=".7" fill="#526064"/>' +
        (fill > 0 ? '<rect x="' + x + '" y="' + y + '" width="' + fill + '" height="3.6" rx=".6" fill="' + color + '"/>' : "");
    }).join("");
  });
  return '<svg class="scale" aria-hidden="true" viewBox="0 0 37 9">' + rows.join("") + '</svg>';
}
function tile(a, style = mode) {
  const logo = a.provider === "Claude" ? "claude" : "openai";
  const resets = style === "hands" ? { week: timeRatio(a, "week"), short: timeRatio(a, "short") } : undefined;
  return '<span class="tile" data-mode="' + style + '">' + dialPreview(a.used, logo, resets) +
    '<span class="side"><span class="alias">' + a.alias + '</span>' +
    (style === "scale" ? timeScale(a) : "") + '</span></span>';
}
function draw() {
  elapsed = advance + Math.max(0, Math.floor((Date.now() - startedAt) / 60000));
  const a = accounts[selected];
  $("#zoom").innerHTML = tile(a);
  $("#who").textContent = a.provider + " · " + a.alias;
  $("#readings").innerHTML = bandKeys(a).map(key =>
    '<tr><td><i class="' + key + '"></i>' + rings[key].name + '</td><td>' + a.used[key] + '%</td><td>' +
    readoutTime(a, key) + '</td></tr>'
  ).join("");
  document.querySelectorAll("#strip button").forEach((b, i) => {
    b.setAttribute("aria-pressed", String(i === selected));
    const item = accounts[i];
    b.title = bandKeys(item).map(k => rings[k].name + " " + item.used[k] + "% 사용 · " +
      readoutTime(item, k)).join("\n");
    b.innerHTML = tile(item);
  });
  document.querySelectorAll(".option").forEach(b => {
    b.setAttribute("aria-pressed", String(b.dataset.mode === mode));
    b.querySelector(".sample-tile").innerHTML = tile(a, b.dataset.mode);
  });
  const now = new Date(base.getTime() + elapsed * 60000);
  $("#clock").textContent = now.toTimeString().slice(0, 5);
  $("#date").textContent = (now.getMonth() + 1) + "." + now.getDate();
  for (const key of Object.keys(rings)) {
    const input = $("#" + key);
    input.closest(".slider").hidden = !Number.isFinite(a.used[key]);
    input.value = a.used[key] ?? 0;
    $("#" + key + "-value").textContent = input.value + "%";
  }
  $("#rewind").disabled = elapsed === 0;
  $("#elapsed").textContent = elapsed > 0 ? duration(elapsed) + " 경과" : "";
}
accounts.forEach((a, i) => {
  const b = document.createElement("button");
  b.setAttribute("aria-label", a.provider + " " + a.alias);
  b.onclick = () => { selected = i; draw(); };
  $("#strip").append(b);
});
document.querySelectorAll(".option").forEach(b => b.onclick = () => { mode = b.dataset.mode; draw(); });
document.querySelectorAll("[data-step]").forEach(b => b.onclick = () => { advance += Number(b.dataset.step); draw(); });
$("#rewind").onclick = () => { advance = 0; startedAt = Date.now(); draw(); };
for (const key of Object.keys(rings)) {
  $("#" + key).oninput = (e) => { accounts[selected].used[key] = Number(e.target.value); draw(); };
}
draw();
setInterval(draw, 60000);
