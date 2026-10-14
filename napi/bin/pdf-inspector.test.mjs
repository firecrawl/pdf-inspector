import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const cli = fileURLToPath(new URL("./pdf-inspector.mjs", import.meta.url));

test("rejects malformed page numbers", () => {
  const result = spawnSync(process.execPath, [cli, "missing.pdf", "--pages", "1foo"], {
    encoding: "utf8",
  });

  assert.equal(result.status, 1);
  assert.match(result.stderr, /invalid page number: 1foo/);
});
