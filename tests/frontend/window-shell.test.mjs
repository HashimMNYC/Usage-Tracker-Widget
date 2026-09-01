import test from "node:test";
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";

test("the main window capability permits the declared drag region", async () => {
  const capability = JSON.parse(await readFile(
    new URL("../../src-tauri/capabilities/default.json", import.meta.url),
    "utf8"
  ));
  const html = await readFile(new URL("../../ui/index.html", import.meta.url), "utf8");
  const styles = await readFile(new URL("../../ui/styles.css", import.meta.url), "utf8");

  assert.deepEqual(capability.permissions, [
    "core:default",
    "core:window:allow-start-dragging"
  ]);
  assert.match(html, /<header[^>]+data-tauri-drag-region/);
  assert.match(html, /class="drag-region"[^>]+data-tauri-drag-region/);
  assert.match(html, /<button id="hide-widget"[^>]+aria-label="Hide usage widget"/);
  assert.match(styles, /\.drag-region\s*\{[\s\S]*?app-region:\s*drag;/);
  assert.match(styles, /button\s*\{[\s\S]*?app-region:\s*no-drag;/);
});
