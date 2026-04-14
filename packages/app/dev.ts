import { watch } from "fs";
import { $ } from "bun";

const PORT = 3000;

async function buildCss() {
  await $`bunx @tailwindcss/cli -i src/index.css -o dist/assets/style.css`.quiet();
}

async function buildJs() {
  const result = await Bun.build({
    entrypoints: ["src/main.tsx"],
    outdir: "dist",
    splitting: true,
    sourcemap: "linked",
    target: "browser",
    naming: "assets/[name].[ext]",
  });
  if (!result.success) {
    for (const msg of result.logs) {
      console.error(msg);
    }
  }
  return result;
}

// Initial build
await $`mkdir -p dist/assets`;
const result = await buildJs();
await buildCss();
await Bun.write("dist/favicon.svg", Bun.file("../../icon.svg"));

const jsEntry = result.outputs.find((o) => o.kind === "entry-point");
const jsPath = jsEntry
  ? jsEntry.path.split("dist/")[1]
  : "assets/main.js";

const indexHtml = `<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <link rel="preconnect" href="https://fonts.googleapis.com" />
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin />
    <link href="https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;500;600;700&display=swap" rel="stylesheet" />
    <link rel="icon" type="image/svg+xml" href="/favicon.svg" />
    <link rel="stylesheet" href="/assets/style.css" />
    <title>concat.me</title>
  </head>
  <body>
    <div id="root"></div>
    <script type="module" src="/${jsPath}"></script>
  </body>
</html>
`;

await Bun.write("dist/index.html", indexHtml);

// Watch for changes and rebuild
let rebuilding = false;
const watchTargets = [
  "src",
  "../core/src",
  "../github/src",
  "../message/src",
];
async function rebuild() {
  if (rebuilding) return;
  rebuilding = true;
  try {
    await Promise.all([buildJs(), buildCss()]);
    console.log(`[${new Date().toLocaleTimeString()}] Rebuilt`);
  } catch (e) {
    console.error("Build error:", e);
  }
  rebuilding = false;
}
for (const target of watchTargets) {
  watch(target, { recursive: true }, rebuild);
}

// Serve
Bun.serve({
  port: PORT,
  async fetch(req) {
    const url = new URL(req.url);
    let filePath = `dist${url.pathname}`;

    // Try exact file first
    const file = Bun.file(filePath);
    if (await file.exists()) {
      return new Response(file);
    }

    // Try with index.html for directories
    const indexFile = Bun.file(`${filePath}/index.html`);
    if (await indexFile.exists()) {
      return new Response(indexFile);
    }

    // SPA fallback
    return new Response(Bun.file("dist/index.html"));
  },
});

console.log(`Dev server running at http://localhost:${PORT}`);
