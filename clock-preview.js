"use strict";

const accounts = [
  { provider: "Claude", alias: "개인", week: 64, short: 78, fable: 41, weekReset: 4 * 1440 + 360, shortReset: 135 },
  { provider: "GPT", alias: "Plus", week: 30, short: 86, weekReset: 1440 + 180, shortReset: 45 },
  { provider: "GPT", alias: "Pro 200", week: 92, short: 38, weekReset: 6 * 1440 + 720, shortReset: 255 },
];
const rings = {
  week: { name: "주간", color: "#b9a5ff", radius: 35, cells: 7, minutes: 7 * 1440 },
  short: { name: "5시간", color: "#65ddd1", radius: 26.5, cells: 5, minutes: 300 },
  fable: { name: "Fable", color: "#ffb784", radius: 19, cells: 1 },
};
const base = new Date(2026, 10, 26, 10, 24);
const $ = (selector) => document.querySelector(selector);
let mode = "scale", selected = 0, elapsed = 0;
let startedAt = Date.now(), advance = 0;

function duration(minutes) {
  if (minutes <= 0) return "갱신 대기";
  const days = Math.floor(minutes / 1440), hours = Math.floor(minutes % 1440 / 60);
  return [days && days + "일", hours && hours + "시간", minutes % 60 && minutes % 60 + "분"].filter(Boolean).join(" ");
}
function timeRatio(a, key) {
  return Math.max(0, Math.min(1, (a[key + "Reset"] - elapsed) / rings[key].minutes));
}
function timeScale(a) {
  const rows = ["week", "short"].map((key, row) => {
    const { cells, color } = rings[key];
    const width = (37 - (cells - 1)) / cells;
    return Array.from({ length: cells }, (_, i) => {
      const fill = width * Math.min(1, Math.max(0, timeRatio(a, key) * cells - i));
      const x = i * (width + 1), y = row * 5.4;
      return '<rect x="' + x + '" y="' + y + '" width="' + width + '" height="3.6" rx=".7" fill="#526064"/>' +
        (fill > 0 ? '<rect x="' + x + '" y="' + y + '" width="' + fill + '" height="3.6" rx=".6" fill="' + color + '"/>' : "");
    }).join("");
  });
  return '<svg class="scale" aria-hidden="true" viewBox="0 0 37 9">' + rows.join("") + '</svg>';
}
function point(radius, fraction) {
  const angle = 2 * Math.PI * fraction;
  return [40 + radius * Math.sin(angle), 40 - radius * Math.cos(angle)];
}
function ring(a, key) {
  const { radius, cells, color } = rings[key];
  const value = a[key] / 100;
  const stroke = key === "fable" ? 3 : 5.2;
  const track = "#505e62";
  if (cells === 1) {
    const circle = '<circle cx="40" cy="40" r="' + radius + '" fill="none" stroke-width="' + stroke + '"';
    return circle + ' stroke="' + track + '"/>' + (value > 0 ? circle + ' stroke="' + color + '" transform="rotate(-90 40 40)" pathLength="100" stroke-dasharray="' + value * 100 + ' 100"/>' : "");
  }
  const gap = 0.011;
  return Array.from({ length: cells }, (_, i) => {
    const start = point(radius, i / cells + gap / 2);
    const end = point(radius, (i + 1) / cells - gap / 2);
    const path = '<path d="M' + start.join(" ") + ' A' + radius + ' ' + radius + ' 0 0 1 ' + end.join(" ") + '" pathLength="100" fill="none" stroke-width="' + stroke + '"';
    const fill = 100 * Math.min(1, Math.max(0, value * cells - i));
    return path + ' stroke="' + track + '"/>' + (fill > 0 ? path + ' stroke="' + color + '" stroke-dasharray="' + fill + ' 100"/>' : "");
  }).join("");
}
function hand(a, key) {
  const { radius, color } = rings[key];
  const left = timeRatio(a, key);
  const [x, y] = point(radius, left);
  // A hollow marker at noon distinguishes an elapsed window from a full new period.
  const fill = left > 0 ? color : "#172123";
  const stroke = left > 0 ? "#10191c" : color;
  return key === "week"
    ? '<path transform="translate(' + x + ' ' + y + ')" d="M0 -5.3 5.3 0 0 5.3 -5.3 0Z" fill="' + fill + '" stroke="' + stroke + '" stroke-width="1.8"/>'
    : '<circle cx="' + x + '" cy="' + y + '" r="4.4" fill="' + fill + '" stroke="' + stroke + '" stroke-width="1.8"/>';
}
function tile(a, style = mode) {
  const logo = a.provider === "Claude" ? "claude" : "openai";
  const size = a.fable == null ? 33 : 27;
  const dial = Object.keys(rings).filter(k => a[k] != null).map(k => ring(a, k)).join("");
  return '<span class="tile" data-mode="' + style + '"><svg class="dial" viewBox="0 0 80 80" aria-hidden="true">' + dial +
    '<image href="' + logo + '.svg" x="' + (80 - size) / 2 + '" y="' + (80 - size) / 2 + '" width="' + size + '" height="' + size + '" style="filter:brightness(0) invert(95%)"/>' +
    (style === "hands" ? hand(a, "week") + hand(a, "short") : "") + '</svg><span class="side"><span class="alias">' + a.alias + '</span>' +
    (style === "scale" ? timeScale(a) : "") + '</span></span>';
}
function draw() {
  elapsed = advance + Math.max(0, Math.floor((Date.now() - startedAt) / 60000));
  const a = accounts[selected];
  $("#zoom").innerHTML = tile(a);
  $("#who").textContent = a.provider + " · " + a.alias;
  $("#readings").innerHTML = Object.keys(rings).filter(k => a[k] != null).map(key =>
    '<tr><td><i class="' + key + '"></i>' + rings[key].name + '</td><td>' + a[key] + '%</td><td>' +
    duration((key === "short" ? a.shortReset : a.weekReset) - elapsed) + '</td></tr>'
  ).join("");
  document.querySelectorAll("#strip button").forEach((b, i) => {
    b.setAttribute("aria-pressed", String(i === selected));
    const item = accounts[i];
    b.title = ["week", "short"].map(k => rings[k].name + " " + item[k] + "% · " + duration(item[k + "Reset"] - elapsed)).join("\n");
    b.innerHTML = tile(item);
  });
  document.querySelectorAll(".option").forEach(b => {
    b.setAttribute("aria-pressed", String(b.dataset.mode === mode));
    b.querySelector(".sample-tile").innerHTML = tile(a, b.dataset.mode);
  });
  const now = new Date(base.getTime() + elapsed * 60000);
  $("#clock").textContent = now.toTimeString().slice(0, 5);
  $("#date").textContent = (now.getMonth() + 1) + "." + now.getDate();
  for (const key of ["week", "short"]) {
    $("#" + key).value = a[key];
    $("#" + key + "-value").textContent = a[key] + "%";
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
for (const key of ["week", "short"]) {
  $("#" + key).oninput = (e) => { accounts[selected][key] = Number(e.target.value); draw(); };
}
draw();
setInterval(draw, 60000);
