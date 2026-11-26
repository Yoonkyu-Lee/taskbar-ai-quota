"use strict";

const dialBands = { week: 7, fable: 7, short: 5 };
const dialOuterRadius = 39, dialStroke = 8, handWidth = 4;

function dialBand(key, value, index) {
  const cells = dialBands[key];
  const radius = dialOuterRadius - (index + .5) * dialStroke;
  const point = fraction => {
    const angle = 2 * Math.PI * fraction;
    return [40 + radius * Math.sin(angle), 40 - radius * Math.cos(angle)];
  };
  const gap = 0.011;
  const paths = Array.from({ length: cells }, (_, i) => {
    const start = point(i / cells + gap / 2), end = point((i + 1) / cells - gap / 2);
    const attrs = `d="M${start.join(" ")} A${radius} ${radius} 0 0 1 ${end.join(" ")}" pathLength="100" stroke-width="${dialStroke}"`;
    const fill = Math.max(0, Math.min(100, value * cells - i * 100));
    return `<path ${attrs} class="track"/>` +
      (fill > 0 ? `<path ${attrs} class="used" stroke-dasharray="${fill} 100"/>` : "");
  }).join("");
  return `<g class="dial-ring ${key}">${paths}</g>`;
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
  const keys = Object.keys(dialBands).filter(key => Number.isFinite(usage[key]));
  const size = keys.length === 3 ? 27 : 33;
  const bands = keys.map((key, index) => dialBand(key, usage[key], index)).join("");
  const shortIndex = keys.indexOf("short");
  const weeklyDepth = (keys.length - Number(shortIndex >= 0)) * dialStroke;
  const shortEdge = dialOuterRadius - shortIndex * dialStroke;
  const hands = (weeklyDepth ? dialHand("week", resets?.week, dialOuterRadius, dialOuterRadius - weeklyDepth) : "") +
    (shortIndex >= 0 ? dialHand("short", resets?.short, shortEdge, shortEdge - dialStroke) : "");
  return `<svg class="dial" viewBox="0 0 80 80" aria-hidden="true">${bands}` +
    `<image class="dial-logo" href="${logo}.svg" x="${(80 - size) / 2}" y="${(80 - size) / 2}" width="${size}" height="${size}"/>${hands}</svg>`;
}
