// Regenerate artwork: node scripts/generate-logo.mjs assets/logo crates/bkpr-client/assets
import { execFileSync } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const [outDir, clientAssets] = process.argv.slice(2);
mkdirSync(outDir, { recursive: true });

const THEMES = {
  light: { ink: "#1A1E26", honey: "#E8990B", fill: "#F0F0F0" },
  dark: { ink: "#F1ECE0", honey: "#F2A516", fill: "#242424" },
};

const beeView = "-9 -11 18 18";

function bee({ ink, fill }, body = fill) {
  return `<defs><clipPath id="body"><ellipse cx="0" cy="1" rx="5.6" ry="4.6"/></clipPath></defs>
  <g fill="${fill}" stroke="${ink}" stroke-width="0.65">
    <ellipse cx="-1.7" cy="-4.4" rx="2.6" ry="4.3" transform="rotate(-14 -1.7 -4.4)"/>
    <ellipse cx="3.1" cy="-3.6" rx="2.6" ry="4.1" transform="rotate(40 3.1 -3.6)"/>
  </g>
  <ellipse cx="0" cy="1" rx="5.6" ry="4.6" fill="${body}" stroke="${ink}" stroke-width="0.65"/>
  <g fill="${ink}" clip-path="url(#body)">
    <rect x="-4.3" y="-5" width="1.5" height="12" transform="rotate(-6 -3.55 1)"/>
    <rect x="-1.3" y="-5" width="1.7" height="12" transform="rotate(-6 -0.45 1)"/>
    <circle cx="3.2" cy="1.3" r="0.8"/>
  </g>`;
}

const r2 = (n) => Math.round(n * 100) / 100;

// Pointy-top hexagon outline as a polygon point list.
function hexPoints(cx, cy, r) {
  const h = 0.866 * r;
  return [[cx, cy - r], [cx + h, cy - r / 2], [cx + h, cy + r / 2], [cx, cy + r], [cx - h, cy + r / 2], [cx - h, cy - r / 2]]
    .map(([x, y]) => `${r2(x)},${r2(y)}`).join(" ");
}

function placedBee(t, x, y, rot, scale) {
  return `<g transform="translate(${x} ${y}) rotate(${rot}) scale(${scale})">${bee({ ink: t.ink, fill: t.fill })}</g>`;
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
    + placedBee(t, r2(mx + 14), r2(my - 23), -25, 1);
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
    <path d="M${k} 14.5 V83.5 M${p} 49 V109.5 M${rr} 36 V83.5" stroke-width="9"/>
    <path d="M${k} 70 L128.5 44 M112.5 59 L130.5 83.5" stroke-width="9"/>
    <polygon points="${hexPoints(pr, 62, 26)}" stroke-width="9"/>
    <path d="M${rr} 49 L246.02 36 L255.5 41.47" stroke-width="9"/>
  </g>
  ${trail(t, loopTrail(), 2.2, "3.5 4")}
  ${placedBee(t, 240, 10, 20, 1.7)}`;
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

if (clientAssets) {
  // Sidebar project colors tint alpha only; black details in the mask become cutouts.
  const mask = `<mask id="bee" maskUnits="userSpaceOnUse" x="-9" y="-11" width="18" height="18">
    ${bee({ ink: "#000", fill: "#fff" })}
  </mask>
  <rect x="-9" y="-11" width="18" height="18" mask="url(#bee)"/>`;
  writeFileSync(join(clientAssets, "bee.svg"), doc(beeView, 18, 18, mask));

  const color = bee({ ink: "#20242C", fill: "#F1ECE0" }, "#F2A516");
  execFileSync("magick", [
    "-background", "none", "svg:-", "-resize", "256x256", "-strip",
    `PNG32:${join(clientAssets, "bkpr-notification.png")}`,
  ], { input: doc(beeView, 512, 512, color) });
}
