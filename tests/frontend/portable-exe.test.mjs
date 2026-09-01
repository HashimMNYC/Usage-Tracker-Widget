import test from "node:test";
import assert from "node:assert/strict";
import {mkdtempSync, rmSync, writeFileSync} from "node:fs";
import {tmpdir} from "node:os";
import {dirname, join, resolve} from "node:path";
import {fileURLToPath} from "node:url";
import {spawnSync} from "node:child_process";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "../..");
const checker = join(repository, "scripts", "check-portable-exe.ps1");
const powershell = join(
  process.env.SystemRoot ?? "C:\\Windows",
  "System32", "WindowsPowerShell", "v1.0", "powershell.exe"
);
const windowsTest = process.platform === "win32" ? test : test.skip;

function portableFixture({
  peOffset = 0x80,
  machine = 0x8664,
  optionalSize = 0xf0,
  magic = 0x020b,
  subsystem = 2,
  dataDirectoryCount = 16,
  fileSize = 0x200
} = {}) {
  const bytes = Buffer.alloc(fileSize);
  bytes.writeUInt16LE(0x5a4d, 0);
  if (peOffset + 24 <= bytes.length) {
    bytes.writeUInt32LE(0x00004550, peOffset);
    bytes.writeUInt16LE(machine, peOffset + 4);
    bytes.writeUInt16LE(optionalSize, peOffset + 20);
    const optional = peOffset + 24;
    if (optional + 2 <= bytes.length) bytes.writeUInt16LE(magic, optional);
    if (optional + 70 <= bytes.length) bytes.writeUInt16LE(subsystem, optional + 68);
    if (optional + 112 <= bytes.length) {
      bytes.writeUInt32LE(dataDirectoryCount, optional + 108);
    }
  }
  bytes.writeUInt32LE(peOffset, 0x3c);
  return bytes;
}

function runChecker(bytes) {
  const directory = mkdtempSync(join(tmpdir(), "usage-widget-pe-"));
  const fixture = join(directory, "fixture.exe");
  try {
    writeFileSync(fixture, bytes);
    const result = spawnSync(
      powershell,
      ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", checker, "-Path", fixture],
      {encoding: "utf8"}
    );
    assert.doesNotMatch(`${result.stdout}${result.stderr}`, new RegExp(fixture.replaceAll("\\", "\\\\"), "i"));
    return result;
  } finally {
    rmSync(directory, {recursive: true, force: true});
  }
}

function assertRejected(bytes) {
  const result = runChecker(bytes);
  assert.equal(result.status, 1);
  assert.match(result.stderr, /^PORTABLE EXE CHECK FAILED:/);
}

windowsTest("accepts a complete AMD64 PE32+ Windows GUI fixture", () => {
  const result = runChecker(portableFixture());
  assert.equal(result.status, 0);
  assert.equal(
    result.stdout.replaceAll("\r\n", "\n"),
    "PE_MACHINE=0x8664\nPE_SUBSYSTEM=2\nPORTABLE_EXE_CHECK=PASS\n"
  );
  assert.equal(result.stderr, "");
});

windowsTest("rejects an e_lfanew below the complete DOS header", () => {
  assertRejected(portableFixture({peOffset: 0x20}));
});

windowsTest("rejects a PE and COFF header that extends beyond the file", () => {
  const bytes = Buffer.alloc(0x80);
  bytes.writeUInt16LE(0x5a4d, 0);
  bytes.writeUInt32LE(0x78, 0x3c);
  assertRejected(bytes);
});

windowsTest("rejects a declared optional header below the PE32+ minimum", () => {
  assertRejected(portableFixture({optionalSize: 0x46}));
});

windowsTest("rejects a file truncated before its declared optional header ends", () => {
  assertRejected(portableFixture({fileSize: 0x80 + 24 + 0xef}));
});

windowsTest("rejects a non-PE32+ optional-header magic", () => {
  assertRejected(portableFixture({magic: 0x010b}));
});

windowsTest("rejects a non-AMD64 machine", () => {
  assertRejected(portableFixture({machine: 0x014c}));
});

windowsTest("rejects a non-GUI subsystem", () => {
  assertRejected(portableFixture({subsystem: 3}));
});

windowsTest("rejects data directories whose declared extent exceeds the optional header", () => {
  assertRejected(portableFixture({optionalSize: 0x70, dataDirectoryCount: 1}));
});
