import test from "node:test";
import assert from "node:assert/strict";
import {readFile} from "node:fs/promises";

test("the whole widget is a native drag surface except for the close button", async () => {
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
  assert.match(html, /<body[^>]+data-tauri-drag-region="deep"/);
  assert.match(
    html,
    /<button id="hide-widget"[^>]+data-tauri-drag-region="false"[^>]+aria-label="Hide usage widget"/
  );
  assert.doesNotMatch(html, /<header[^>]+data-tauri-drag-region/);
  assert.doesNotMatch(html, /class="drag-region"[^>]+data-tauri-drag-region/);
  assert.match(styles, /body\[data-tauri-drag-region="deep"\]\s*\{[\s\S]*?cursor:\s*grab;/);
  assert.match(styles, /body\[data-tauri-drag-region="deep"\]:active\s*\{[\s\S]*?cursor:\s*grabbing;/);
  assert.doesNotMatch(styles, /(?:-webkit-)?app-region\s*:/);
});
