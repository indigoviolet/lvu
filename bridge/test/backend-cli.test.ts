import { chmod, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { PaseoCli } from "../src/backend.js";

describe("Paseo CLI cancellation fallback", () => {
  it("probes availability and passes the exact agent and SDK daemon endpoint as arguments", async () => {
    const directory = await mkdtemp(join(tmpdir(), "lvu-paseo-cli-"));
    const executable = join(directory, "fake-paseo");
    const captured = join(directory, "args.json");
    await writeFile(executable, `#!/usr/bin/env node\nconst fs=require("node:fs");const args=process.argv.slice(2);if(args[0]==="--version"){process.stdout.write("0.7.2\\n");}else{fs.writeFileSync(${JSON.stringify(captured)},JSON.stringify(args));process.stdout.write(JSON.stringify({stoppedCount:1}));}\n`);
    await chmod(executable, 0o700);
    try {
      const cli = new PaseoCli(executable, "127.0.0.1:6767");
      expect(cli.available).toBe(false);
      await cli.probe(1_000);
      expect(cli.available).toBe(true);
      await expect(cli.cancel("agent-with-spaces ; ignored", 1_000)).resolves.toBe(true);
      expect(JSON.parse(await readFile(captured, "utf8"))).toEqual(["stop", "agent-with-spaces ; ignored", "--host", "127.0.0.1:6767", "--json"]);
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
});
