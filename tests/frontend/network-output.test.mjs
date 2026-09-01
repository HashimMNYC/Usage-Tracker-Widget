import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import test from "node:test";

const scriptPath = fileURLToPath(
  new URL("../../scripts/check-no-network.ps1", import.meta.url),
);

const testWriterDefinition = String.raw`
Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Text;

public sealed class NetworkTestWriter : TextWriter
{
    private readonly StringBuilder content = new StringBuilder();

    public int FailWriteAt { get; set; }
    public bool FailFlush { get; set; }
    public int WriteCalls { get; private set; }
    public int FlushCalls { get; private set; }
    public string Content { get { return content.ToString(); } }
    public override Encoding Encoding { get { return Encoding.UTF8; } }

    public override void WriteLine(string value)
    {
        WriteCalls += 1;
        if (FailWriteAt == WriteCalls)
        {
            throw new IOException("synthetic write failure");
        }
        if (content.Length > 0)
        {
            content.Append('\n');
        }
        content.Append(value);
    }

    public override void Flush()
    {
        FlushCalls += 1;
        if (FailFlush)
        {
            throw new IOException("synthetic flush failure");
        }
    }
}
'@
`;

function quotePowerShellLiteral(value) {
  return `'${value.replaceAll("'", "''")}'`;
}

function runPowerShell(lines, { withTestWriter = false } = {}) {
  const command = [
    `. ${quotePowerShellLiteral(scriptPath)} -RootPid 1`,
    ...(withTestWriter ? [testWriterDefinition] : []),
    ...lines,
  ].join("\n");

  return spawnSync(
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
}

function resultLines() {
  return [
    "$connections = @(",
    "  [pscustomobject][ordered]@{ PID = 7; State = 'Established'; RemoteAddress = '203.0.113.1' }",
    "  [pscustomobject][ordered]@{ PID = 8; State = 'SynSent'; RemoteAddress = '2001:db8::8' }",
    ")",
    "$result = New-NetworkSampleResult -Complete $true -Connections $connections",
  ];
}

function writerSummaryLines() {
  return [
    "$summary = [pscustomobject][ordered]@{ code = $code; content = $writer.Content; writes = $writer.WriteCalls; flushes = $writer.FlushCalls }",
    "$summary | ConvertTo-Json -Compress",
    "exit $code",
  ];
}

function parseSummary(result) {
  assert.equal(result.stderr, "");
  return JSON.parse(result.stdout.trim());
}

test("network findings remain visible when the checker exits 1", () => {
  const result = runPowerShell([
    "$connection = [pscustomobject][ordered]@{ PID = 7; State = 'Established'; RemoteAddress = '203.0.113.1' }",
    "$sample = New-NetworkSampleResult -Complete $true -Connections @($connection)",
    "$code = Write-NetworkSampleResult -Result $sample",
    "exit $code",
  ]);

  assert.equal(result.status, 1);
  assert.equal(result.stderr, "");
  assert.equal(result.stdout.trimEnd(), "7\tEstablished\t203.0.113.1");
});

test("multiple findings are written and flushed in exact order before exit 1", () => {
  const result = runPowerShell(
    [
      ...resultLines(),
      "$writer = [NetworkTestWriter]::new()",
      "$code = Write-NetworkSampleResult -Result $result -Writer $writer",
      ...writerSummaryLines(),
    ],
    { withTestWriter: true },
  );

  assert.equal(result.status, 1);
  assert.deepEqual(parseSummary(result), {
    code: 1,
    content: "7\tEstablished\t203.0.113.1\n8\tSynSent\t2001:db8::8",
    writes: 2,
    flushes: 1,
  });
});

for (const failure of [
  { name: "before the first finding", failWriteAt: 1, content: "" },
  {
    name: "after one finding",
    failWriteAt: 2,
    content: "7\tEstablished\t203.0.113.1",
  },
]) {
  test(`a write failure ${failure.name} maps to exit 2 without raw errors`, () => {
    const result = runPowerShell(
      [
        ...resultLines(),
        "$writer = [NetworkTestWriter]::new()",
        `$writer.FailWriteAt = ${failure.failWriteAt}`,
        "$code = Write-NetworkSampleResult -Result $result -Writer $writer",
        ...writerSummaryLines(),
      ],
      { withTestWriter: true },
    );

    assert.equal(result.status, 2);
    assert.deepEqual(parseSummary(result), {
      code: 2,
      content: failure.content,
      writes: failure.failWriteAt,
      flushes: 0,
    });
  });
}

test("a flush failure maps to exit 2 without raw errors", () => {
  const result = runPowerShell(
    [
      ...resultLines(),
      "$writer = [NetworkTestWriter]::new()",
      "$writer.FailFlush = $true",
      "$code = Write-NetworkSampleResult -Result $result -Writer $writer",
      ...writerSummaryLines(),
    ],
    { withTestWriter: true },
  );

  assert.equal(result.status, 2);
  assert.deepEqual(parseSummary(result), {
    code: 2,
    content: "7\tEstablished\t203.0.113.1\n8\tSynSent\t2001:db8::8",
    writes: 2,
    flushes: 1,
  });
});

test("a complete empty sample exits 0 without touching the writer", () => {
  const result = runPowerShell(
    [
      "$result = New-NetworkSampleResult -Complete $true",
      "$writer = [NetworkTestWriter]::new()",
      "$code = Write-NetworkSampleResult -Result $result -Writer $writer",
      ...writerSummaryLines(),
    ],
    { withTestWriter: true },
  );

  assert.equal(result.status, 0);
  assert.deepEqual(parseSummary(result), {
    code: 0,
    content: "",
    writes: 0,
    flushes: 0,
  });
});

test("an incomplete sample exits 2 without touching the writer", () => {
  const result = runPowerShell(
    [
      "$result = New-NetworkSampleResult -Complete $false",
      "$writer = [NetworkTestWriter]::new()",
      "$code = Write-NetworkSampleResult -Result $result -Writer $writer",
      ...writerSummaryLines(),
    ],
    { withTestWriter: true },
  );

  assert.equal(result.status, 2);
  assert.deepEqual(parseSummary(result), {
    code: 2,
    content: "",
    writes: 0,
    flushes: 0,
  });
});
