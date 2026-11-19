// Generates the bkpr logo SVGs in assets/logo: `node scripts/generate-logo.mjs assets/logo`.
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const outDir = process.argv[2];
mkdirSync(outDir, { recursive: true });

const THEMES = {
  light: { ink: "#1A1E26", honey: "#E8990B", stripe: "#1A1E26" },
  dark: { ink: "#F1ECE0", honey: "#F2A516", stripe: "#1A1E26" },
};

const r2 = (n) => Math.round(n * 100) / 100;

// Pointy-top hexagon outline as a polygon point list.
function hexPoints(cx, cy, r) {
  const h = 0.866 * r;
  return [[cx, cy - r], [cx + h, cy - r / 2], [cx + h, cy + r / 2], [cx, cy + r], [cx - h, cy + r / 2], [cx - h, cy - r / 2]]
    .map(([x, y]) => `${r2(x)},${r2(y)}`).join(" ");
}

function bee(t, x, y, rot, scale) {
  return `<g transform="translate(${x} ${y}) rotate(${rot}) scale(${scale})">
    <g fill="none" stroke="${t.ink}" stroke-width="1.8">
      <ellipse cx="-3" cy="-5" rx="3.2" ry="5" transform="rotate(-25 -3 -5)"/>
      <ellipse cx="3" cy="-5" rx="3.2" ry="5" transform="rotate(25 3 -5)"/>
    </g>
    <clipPath id="bee-body"><ellipse cx="0" cy="1" rx="5.5" ry="4.2"/></clipPath>
    <ellipse cx="0" cy="1" rx="5.5" ry="4.2" fill="${t.honey}"/>
    <rect x="-1.1" y="-4" width="2.2" height="10" fill="${t.stripe}" clip-path="url(#bee-body)"/>
  </g>`;
}

const trail = (t, d, width = 2, dash = "3 3.5") =>
  `<path d="${d}" fill="none" stroke="${t.ink}" stroke-width="${width}" stroke-dasharray="${dash}" stroke-linecap="round"/>`;

// The hex b: stem shares the cell's left wall. `prompt` is "full", "underscore" or "none".
function hexB(t, { cx = 54, cy = 62, r = 26, stemTop = 10, prompt = "full" } = {}) {
  const h = 0.866 * r;
  const stemX = cx - h;
  // One continuous stroke: stem down the cell's left wall, then around the cell.
  const d = `M${r2(stemX)} ${stemTop + 4.5} V${r2(cy + r / 2)} L${cx} ${cy + r} L${r2(cx + h)} ${r2(cy + r / 2)} V${r2(cy - r / 2)} L${cx} ${cy - r} L${r2(stemX)} ${r2(cy - r / 2)}`;
  let out = `<path d="${d}" fill="none" stroke="${t.ink}" stroke-width="9" stroke-linecap="round" stroke-linejoin="round"/>`;
  if (prompt === "full") {
    out += `
  <path d="M${cx - 11} ${cy - 7} l7 7 l-7 7" fill="none" stroke="${t.ink}" stroke-width="4.5" stroke-linecap="round" stroke-linejoin="round"/>
  <path d="M${cx} ${cy + 7} H${cx + 11}" stroke="${t.honey}" stroke-width="4.5" stroke-linecap="round"/>`;
  } else if (prompt === "underscore") {
    out += `
  <path d="M${cx - 9} ${cy + 7} H${cx + 9}" stroke="${t.honey}" stroke-width="7.2" stroke-linecap="round"/>`;
  }
  return out;
}

// Departing bee for the standalone mark, leaving the cell's upper-right face.
function logoBee(t, { cx = 54, cy = 62, r = 26 } = {}) {
  const mx = cx + 0.433 * r, my = cy - 0.75 * r;
  return trail(t, `M${r2(mx + 3.5)} ${r2(my - 6)} C${r2(mx + 9)} ${r2(my - 9)} ${r2(mx + 12.5)} ${r2(my - 12)} ${r2(mx + 12)} ${r2(my - 16)}`)
    + bee(t, r2(mx + 14), r2(my - 23), -25, 1);
}

// Banner flight path: a quadratic arc from the b's cell to the bee, with a prolate-cycloid
// loop-the-loop over the gap between k and p. Sampled, then smoothed with Catmull-Rom cubics.
function loopTrail() {
  const S = [68.8, 36.5], C = [110, -40], E = [226, 8];
  const loop = { from: 0.5, to: 0.61, radius: 14 };
  const at = (t) => {
    const u = 1 - t;
    let x = u * u * S[0] + 2 * t * u * C[0] + t * t * E[0];
    let y = u * u * S[1] + 2 * t * u * C[1] + t * t * E[1];
    if (t > loop.from && t < loop.to) {
      const phi = (2 * Math.PI * (t - loop.from)) / (loop.to - loop.from);
      x += loop.radius * Math.sin(phi);
      y -= loop.radius * (1 - Math.cos(phi));
    }
    return [x, y];
  };
  const n = 120;
  const pts = Array.from({ length: n + 1 }, (_, i) => at(i / n));
  let d = `M${r2(pts[0][0])} ${r2(pts[0][1])}`;
  for (let i = 0; i < n; i++) {
    const p0 = pts[Math.max(i - 1, 0)], p1 = pts[i], p2 = pts[i + 1], p3 = pts[Math.min(i + 2, n)];
    const c1 = [p1[0] + (p2[0] - p0[0]) / 6, p1[1] + (p2[1] - p0[1]) / 6];
    const c2 = [p2[0] - (p3[0] - p1[0]) / 6, p2[1] - (p3[1] - p1[1]) / 6];
    d += ` C${r2(c1[0])} ${r2(c1[1])} ${r2(c2[0])} ${r2(c2[1])} ${r2(p2[0])} ${r2(p2[1])}`;
  }
  return d;
}

// Wordmark: the hex b logo followed by "kpr" drawn on the same grid; the bee flies over the word.
function banner(t) {
  const k = 100.5, p = 154.5, pr = 177, rr = 223.5;
  return `${hexB(t)}
  <g fill="none" stroke="${t.ink}" stroke-linecap="round" stroke-linejoin="round">
    <path d="M${k} 14.5 V83.5 M${p} 49 V109.5 M${rr} 49 V83.5" stroke-width="9"/>
    <path d="M${k} 70 L128.5 44 M112.5 59 L130.5 83.5" stroke-width="9"/>
    <polygon points="${hexPoints(pr, 62, 26)}" stroke-width="9"/>
    <path d="M${rr} 49 L246.02 36 L255.5 41.47" stroke-width="9"/>
  </g>
  ${trail(t, loopTrail(), 2.2, "3.5 4")}
  ${bee(t, 240, 10, 20, 1.7)}`;
}

const doc = (viewBox, width, height, body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}" width="${width}" height="${height}" role="img" aria-labelledby="title">
  <title id="title">bkpr</title>
  ${body}
</svg>
`;

for (const [name, t] of Object.entries(THEMES)) {
  const files = {
    [`bkpr-logo-${name}.svg`]: doc("7 0 100 100", 512, 512, hexB(t) + "\n  " + logoBee(t)),
    [`bkpr-icon-${name}.svg`]: doc("3.75 1.25 100 100", 512, 512, hexB(t)),
    [`bkpr-favicon-${name}.svg`]: doc("9.75 7.25 88 88", 16, 16, hexB(t, { prompt: "underscore" })),
    [`bkpr-banner-${name}.svg`]: doc("6 -48 274 184", 548, 368, banner(t)),
  };
  for (const [file, svg] of Object.entries(files)) writeFileSync(join(outDir, file), svg);
}
