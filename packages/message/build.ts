import { $ } from "bun";

const root = new URL("../../", import.meta.url).pathname;
const outDir = new URL("src/wasm/", import.meta.url).pathname;

await $`cargo build -p concats-message --target wasm32-unknown-unknown --release --features wasm`.cwd(
  root,
);

const wasmPath = `${root}target/wasm32-unknown-unknown/release/concats_message.wasm`;
await $`wasm-bindgen --target web --out-dir ${outDir} ${wasmPath}`;

const wasmBytes = await Bun.file(
  `${outDir}concats_message_bg.wasm`,
).arrayBuffer();
const base64 = Buffer.from(wasmBytes).toString("base64");
await Bun.write(
  `${outDir}inline.ts`,
  `export const wasmBase64 = "${base64}";\n`,
);

console.log("WASM build complete");
