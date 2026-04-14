import { $ } from "bun";

const outdir = "dist";

// Bundle the application
const result = await Bun.build({
  entrypoints: ["src/main.tsx"],
  outdir,
  minify: true,
  splitting: true,
  sourcemap: "external",
  target: "browser",
  naming: "assets/[name]-[hash].[ext]",
});

if (!result.success) {
  for (const msg of result.logs) {
    console.error(msg);
  }
  process.exit(1);
}

// Build CSS with Tailwind CLI
await $`bunx @tailwindcss/cli -i src/index.css -o ${outdir}/assets/style.css --minify`;

// Copy favicon from repo root
await Bun.write(`${outdir}/favicon.svg`, Bun.file("../../icon.svg"));

// Find the JS entry output filename
const jsEntry = result.outputs.find((o) => o.kind === "entry-point");
if (!jsEntry) {
  console.error("No entry-point output found");
  process.exit(1);
}
const jsPath = jsEntry.path.split(`${outdir}/`)[1];

// Write index.html
const html = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <link rel="preconnect" href="https://fonts.googleapis.com" />
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
    <link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600;700&display=swap" rel="stylesheet" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg" />
    <link rel="stylesheet" href="/${jsPath ? "assets/style.css" : ""}" />
    <title>concat.me</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/${jsPath}"></script>
  </body>
</html>
`;

await Bun.write(`${outdir}/index.html`, html);

console.log(`Built to ${outdir}/`);
for (const output of result.outputs) {
  console.log(`  ${output.path}`);
}
