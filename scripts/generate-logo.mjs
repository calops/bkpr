// Regenerate artwork: node scripts/generate-logo.mjs assets/logo crates/bkpr-client/assets
import { execFileSync } from "node:child_process";
import { writeFileSync, mkdirSync } from "node:fs";
import { join } from "node:path";

const [outDir, clientAssets] = process.argv.slice(2);
mkdirSync(outDir, { recursive: true });

const THEMES = {
  light: { ink: "#242424", fill: "#F0F0F0" },
  dark: { ink: "#F0F0F0", fill: "#242424" },
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

function banner(t) {
  return `<g transform="translate(38 64) scale(5)">
    ${bee(t)}
  </g>
  <g fill="none" stroke="${t.ink}" stroke-linecap="round" stroke-linejoin="round" stroke-width="9" transform="translate(68)">
    <path d="M31.5 14.5 V83.5 M31.5 51 L54 36 L76.5 49 V75 L54 88 L31.5 75"/>
    <path d="M100.5 14.5 V83.5 M100.5 70 L128.5 44 M112.5 59 L130.5 83.5"/>
    <path d="M154.5 49 V109.5 M154.5 49 L177 36 L199.5 49 V75 L177 88 L154.5 75"/>
    <path d="M223.5 83.5 V49 L246 36 L255.5 41.5"/>
  </g>`;
}

const doc = (viewBox, width, height, body) =>
  `<svg xmlns="http://www.w3.org/2000/svg" viewBox="${viewBox}" width="${width}" height="${height}" role="img" aria-labelledby="title">
  <title id="title">bkpr</title>
  ${body}
</svg>
`;

for (const [name, t] of Object.entries(THEMES)) {
  const files = {
    [`bkpr-logo-${name}.svg`]: doc(beeView, 512, 512, bee(t)),
    [`bkpr-icon-${name}.svg`]: doc(beeView, 512, 512, bee(t)),
    [`bkpr-favicon-${name}.svg`]: doc(beeView, 16, 16, bee(t)),
    [`bkpr-banner-${name}.svg`]: doc("0 0 340 124", 680, 248, banner(t)),
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
