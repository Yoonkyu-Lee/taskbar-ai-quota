"use strict";

const dialBands = { week: 7, fable: 7, short: 5 };
const dialOuterRadius = 39, dialStroke = 8, handWidth = 4;
// The preview draws at twice the native 40 DIP dial, so 0.3 DIP becomes 0.6 units.
const tickOverlap = 0.6;

function dialBand(key, value, outer, stroke) {
  const cells = dialBands[key];
  const radius = outer - stroke / 2;
  const point = fraction => {
    const angle = 2 * Math.PI * fraction;
    return [40 + radius * Math.sin(angle), 40 - radius * Math.cos(angle)];
  };
  const gap = 0.011;
  const paths = Array.from({ length: cells }, (_, i) => {
    const start = point(i / cells + gap / 2), end = point((i + 1) / cells - gap / 2);
    const attrs = `d="M${start.join(" ")} A${radius} ${radius} 0 0 1 ${end.join(" ")}" pathLength="100" stroke-width="${stroke}"`;
    const fill = Math.max(0, Math.min(100, value * cells - i * 100));
    return `<path ${attrs} class="track"/>` +
      (fill > 0 ? `<path ${attrs} class="used" stroke-dasharray="${fill} 100"/>` : "");
  }).join("");
  // White ticks fill each gap and reach 0.3 DIP into both cells, as in the native renderer.
  const half = gap / 2 + tickOverlap / (2 * Math.PI * radius);
  const ticks = Array.from({ length: cells }, (_, i) => {
    const start = point(i / cells - half), end = point(i / cells + half);
    return `<path class="tick" d="M${start.join(" ")} A${radius} ${radius} 0 0 1 ${end.join(" ")}" stroke-width="${stroke}"/>`;
  }).join("");
  return `<g class="dial-ring ${key}">${paths}${ticks}</g>`;
}

function dialHand(key, left, outer, inner) {
  if (!Number.isFinite(left)) return "";
  const angle = left === 0 ? 0 : (1 - left) * 360;
  // Center the curved caps on the dial so the hand stays within its bands.
  const cap = (o, i) => {
    const half = handWidth / 2;
    const leftX = 40 - half, rightX = 40 + half;
    const top = 40 - Math.sqrt(o * o - half * half);
    const bottom = 40 - Math.sqrt(i * i - half * half);
    return `M${leftX} ${top} A${o} ${o} 0 0 1 ${rightX} ${top} L${rightX} ${bottom} A${i} ${i} 0 0 0 ${leftX} ${bottom} Z`;
  };
  const depth = (outer - inner) / 3;
  const shape = left === 0 ? cap(outer, outer - depth) + " " + cap(inner + depth, inner) : cap(outer, inner);
  return `<g class="reset-hand ${key}${left === 0 ? " expired" : ""}" transform="rotate(${angle} 40 40)">` +
    (left === 0 ? `<path class="hand-gap" d="${cap(outer, inner)}"/>` : "") +
    `<path class="hand-fill" d="${shape}"/></g>`;
}

function dialPreview(usage, logo, resets) {
  const weeklyKeys = ["week", "fable"].filter(key => Number.isFinite(usage[key]));
  const weeklyWidth = weeklyKeys.length === 2 ? dialStroke / 2 : dialStroke;
  const shortEdge = dialOuterRadius - weeklyKeys.length * weeklyWidth;
  const hasShort = Number.isFinite(usage.short);
  const bands = weeklyKeys.map((key, i) => dialBand(key, usage[key], dialOuterRadius - i * weeklyWidth, weeklyWidth)).join("") +
    (hasShort ? dialBand("short", usage.short, shortEdge, dialStroke) : "");
  const hands = (weeklyKeys.length ? dialHand("week", resets?.week, dialOuterRadius, shortEdge) : "") +
    (hasShort ? dialHand("short", resets?.short, shortEdge, shortEdge - dialStroke) : "");
  return `<svg class="dial" viewBox="0 0 80 80" aria-hidden="true">${bands}` +
    `<image class="dial-logo" href="${logo}.svg" x="23.5" y="23.5" width="33" height="33"/>${hands}</svg>`;
}
