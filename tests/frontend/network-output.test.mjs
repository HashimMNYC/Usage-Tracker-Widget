import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const scriptPath = fileURLToPath(
  new URL("../../scripts/check-no-network.ps1", import.meta.url),
);

function quotePowerShellLiteral(value) {
  return `'${value.replaceAll("'", "''")}'`;
}

test("network findings remain visible when the checker exits 1", () => {
  const command = [
    `. ${quotePowerShellLiteral(scriptPath)} -RootPid 1`,
    "$connection = [pscustomobject][ordered]@{ PID = 7; State = 'Established'; RemoteAddress = '203.0.113.1' }",
    "Write-NetworkConnectionRows -Connections @($connection)",
    "exit 1",
  ].join("; ");

  const result = spawnSync(
    "powershell.exe",
    [
      "-NoProfile",
      "-NonInteractive",
      "-ExecutionPolicy",
      "Bypass",
      "-Command",
      command,
    ],
    { encoding: "utf8" },
  );

  assert.equal(result.status, 1);
  assert.equal(result.stderr, "");
  assert.equal(result.stdout.trimEnd(), "7\tEstablished\t203.0.113.1");
});
