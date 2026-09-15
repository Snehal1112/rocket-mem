# TLS Suite Automation — Scratch Cleanup Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the "Clean environment" button, when clicked for the TLS suite, also delete the scratch files TLS's own cases create under `/tmp/acltls-qa` (certs, AOF/snapshot, TOML configs, PID/log files) — not just kill the process on its ports, which is all `POST /api/clear-env` does today for every suite.

**Architecture:** `docs/qa-playbook.md`'s "ACL and TLS: before you start" section intro creates one shared directory, `/tmp/acltls-qa`, that **both** the ACL and TLS suites write into — confirmed by enumerating every literal `/tmp/acltls-qa/...` path referenced in the doc (`grep -oE '/tmp/acltls-qa[a-zA-Z0-9_./-]*' docs/qa-playbook.md | sort -u`). TLS's own files use `tls*`/`cfgdir/`/`acltls*`/`acl-tls.toml` names; ACL's own use `acl.*`/`acl-bad*.toml`/`acl-server.log`. Note the overlap trap: `acl-tls.toml` starts with `acl-` but is TLS-10's own file, and `acltls.aof`/`acltls.pid`/`acltls.snap`/`acltls-server.log` (TLS-10's own, again) all start with `acl` too — a prefix-based glob (`acl*`) would wrongly sweep up TLS's files under an "ACL" cleanup or vice versa. This plan adds an explicit **literal path list per suite** (`SUITE_SCRATCH_PATHS` in `suites.ts`), not a glob, specifically to avoid that trap. Only `"TLS"`'s entry is populated in this plan — `"ACL and authentication"` is left with no entry (no behavior change for that suite; out of scope for what was asked).

**Tech Stack:** Node.js/TypeScript (`tools/qa-agent`), `yarn`.

**Spec:** none (bounded task; continues `docs/superpowers/plans/2026-09-15-tls-suite-automation-2-wiring.md`).

## Global Constraints

- `tools/qa-agent` uses Yarn — every command runs as `yarn <script>` from inside `tools/qa-agent`.
- Every path added to `TLS`'s scratch-path list must be copied verbatim from the enumeration above — do not shorten to a directory-only entry where the doc creates loose files directly in `/tmp/acltls-qa` (there is no single "TLS subdirectory" to `rm -rf` — only `/tmp/acltls-qa/tls` and `/tmp/acltls-qa/cfgdir` are TLS's own subdirectories; the rest are loose files sitting next to ACL's own).
- Never add a glob or prefix match in place of the literal list — that is exactly the mechanism that would delete ACL's files (see Architecture above).
- `tools/` is gitignored — this plan's `suites.ts`/`server.ts`/test changes are all under `tools/qa-agent`, so none of them are committed. (No commit steps appear in this plan for that reason — confirm this if a step ever looks like it should have one.)

---

### Task 1: Add `TLS`'s scratch-path list to `suites.ts`

**Files:**
- Modify: `tools/qa-agent/src/suites.ts`
- Test: `tools/qa-agent/src/suites.test.ts`

**Interfaces:**
- Produces: `SUITE_SCRATCH_PATHS: Record<string, string[]>`, a new export alongside `SUITE_SERVER_PORT`/`SUITE_EXTRA_OWN_SERVER_PORTS`, read by `server.ts`'s `/api/clear-env` handler in Task 2.

- [ ] **Step 1: Write the failing test**

In `tools/qa-agent/src/suites.test.ts`, add:

```typescript
it("TLS's scratch paths are exactly its own files, none of ACL's", () => {
  const tlsPaths = SUITE_SCRATCH_PATHS["TLS"] ?? [];
  expect(tlsPaths.sort()).toEqual(
    [
      "/tmp/acltls-qa/tls",
      "/tmp/acltls-qa/tls.aof",
      "/tmp/acltls-qa/tls.snap",
      "/tmp/acltls-qa/tls.pid",
      "/tmp/acltls-qa/tls-server.log",
      "/tmp/acltls-qa/cfgdir",
      "/tmp/acltls-qa/acl-tls.toml",
      "/tmp/acltls-qa/acltls.aof",
      "/tmp/acltls-qa/acltls.snap",
      "/tmp/acltls-qa/acltls.pid",
      "/tmp/acltls-qa/acltls-server.log",
    ].sort(),
  );
  const aclOwnFiles = [
    "/tmp/acltls-qa/acl.aof",
    "/tmp/acltls-qa/acl.snap",
    "/tmp/acltls-qa/acl.pid",
    "/tmp/acltls-qa/acl.toml",
    "/tmp/acltls-qa/acl-bad.toml",
    "/tmp/acltls-qa/acl-bad2.toml",
    "/tmp/acltls-qa/acl-server.log",
  ];
  for (const f of aclOwnFiles) expect(tlsPaths).not.toContain(f);
});
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```

Expected: FAIL — `SUITE_SCRATCH_PATHS` doesn't exist yet (`TypeError`/`undefined`).

- [ ] **Step 3: Add the export**

In `tools/qa-agent/src/suites.ts`, add after `SUITE_EXTRA_OWN_SERVER_PORTS`:

```typescript
// Literal scratch files/directories a suite's own cases create outside its own process's
// lifecycle — the "Clean environment" button's port-kill (suitePorts, above) only stops the
// server; it was never told to remove what the server left behind on disk. Deliberately an
// explicit literal list, never a glob: TLS and ACL share one directory (/tmp/acltls-qa, created
// once by the "ACL and TLS: before you start" section intro), and a prefix match would sweep up
// the other suite's files — e.g. TLS-10's own acl-tls.toml/acltls.{aof,snap,pid} all start with
// "acl", and a naive "acl*" glob scoped to ACL would delete TLS's files too. Only "TLS" has an
// entry today; ACL's own equivalent is a deliberately separate, unstarted piece of work.
export const SUITE_SCRATCH_PATHS: Record<string, string[]> = {
  TLS: [
    "/tmp/acltls-qa/tls",
    "/tmp/acltls-qa/tls.aof",
    "/tmp/acltls-qa/tls.snap",
    "/tmp/acltls-qa/tls.pid",
    "/tmp/acltls-qa/tls-server.log",
    "/tmp/acltls-qa/cfgdir",
    "/tmp/acltls-qa/acl-tls.toml",
    "/tmp/acltls-qa/acltls.aof",
    "/tmp/acltls-qa/acltls.snap",
    "/tmp/acltls-qa/acltls.pid",
    "/tmp/acltls-qa/acltls-server.log",
  ],
};
```

- [ ] **Step 4: Run the test to verify it passes**

```bash
cd tools/qa-agent && yarn test src/suites.test.ts
```

Expected: PASS.

---

### Task 2: Wire `SUITE_SCRATCH_PATHS` into `/api/clear-env`

**Files:**
- Modify: `tools/qa-agent/src/server.ts` (the `/api/clear-env` handler, `tools/qa-agent/src/server.ts:367`-`411`, and its imports)
- Test: `tools/qa-agent/src/server.test.ts`

**Interfaces:**
- Consumes: `SUITE_SCRATCH_PATHS` from `./suites.js` (Task 1); `rmSync` from `node:fs`.
- Produces: `/api/clear-env`'s JSON response gains a `removedPaths: string[]` field — every path from `SUITE_SCRATCH_PATHS[section]` that actually existed and was removed (paths that were already absent are silently skipped, same "already-free" spirit as the existing `ports` field).

- [ ] **Step 1: Write the failing test**

In `tools/qa-agent/src/server.test.ts`, inside the existing `describe("POST /api/clear-env", ...)` block, add (using Node's `fs` directly to set up/verify scratch files — no real TLS server needed, since this is testing the file-removal mechanism, not TLS's own cases):

```typescript
it("removes a suite's own scratch files, listing them in removedPaths, without touching an unrelated file in the same directory", async () => {
  const fs = await import("node:fs");
  fs.mkdirSync("/tmp/acltls-qa/tls", { recursive: true });
  fs.writeFileSync("/tmp/acltls-qa/tls/cert.pem", "fake");
  fs.writeFileSync("/tmp/acltls-qa/tls.pid", "PID=1");
  fs.writeFileSync("/tmp/acltls-qa/acl.pid", "PID=2"); // a stand-in for ACL's own file

  server = await startServer(0);
  const res = await fetch(`http://127.0.0.1:${server.port}/api/clear-env`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ section: "TLS" }),
  });
  expect(res.status).toBe(200);
  const body = await res.json();
  expect(body.removedPaths).toContain("/tmp/acltls-qa/tls");
  expect(body.removedPaths).toContain("/tmp/acltls-qa/tls.pid");
  expect(body.removedPaths).not.toContain("/tmp/acltls-qa/acl.pid");

  expect(fs.existsSync("/tmp/acltls-qa/tls")).toBe(false);
  expect(fs.existsSync("/tmp/acltls-qa/tls.pid")).toBe(false);
  expect(fs.existsSync("/tmp/acltls-qa/acl.pid")).toBe(true);

  fs.rmSync("/tmp/acltls-qa/acl.pid", { force: true });
});

it("reports an empty removedPaths, not an error, when a suite has no scratch-path entry", async () => {
  server = await startServer(0);
  const res = await fetch(`http://127.0.0.1:${server.port}/api/clear-env`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ section: "Smoke suite" }),
  });
  expect(res.status).toBe(200);
  const body = await res.json();
  expect(body.removedPaths).toEqual([]);
});
```

- [ ] **Step 2: Run the tests to verify they fail**

```bash
cd tools/qa-agent && yarn test src/server.test.ts -t "clear-env"
```

Expected: FAIL — `body.removedPaths` is `undefined`.

- [ ] **Step 3: Implement the removal**

In `tools/qa-agent/src/server.ts`, add `rmSync` to the existing `node:fs` import (line 2) and `SUITE_SCRATCH_PATHS` to the existing `./suites.js` import (lines 7-14). Then, in the `/api/clear-env` handler, right after the `const ports = suitePorts(section)...` line (`server.ts:394`) and before `suiteSessions.delete(section)`, add:

```typescript
      // Port-kill above only stops the process; it never touches what that process left on
      // disk. Remove each of this suite's own scratch files/directories too — see
      // SUITE_SCRATCH_PATHS's own comment in suites.ts for why this is an explicit literal
      // list, not a glob, and why only some suites have an entry.
      const removedPaths: string[] = [];
      for (const p of SUITE_SCRATCH_PATHS[section] ?? []) {
        if (existsSync(p)) {
          rmSync(p, { recursive: true, force: true });
          removedPaths.push(p);
        }
      }
```

Then add `removedPaths` to the response object:

```typescript
      res.end(JSON.stringify({ section, ports, clearedResults, removedPaths }));
```

(replacing the existing `res.end(JSON.stringify({ section, ports, clearedResults }));` line.)

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd tools/qa-agent && yarn test src/server.test.ts -t "clear-env"
```

Expected: PASS.

---

### Task 3: Full regression and a manual sanity check

**Files:** none modified — verification only.

**Interfaces:** N/A.

- [ ] **Step 1: Full regression**

```bash
cd tools/qa-agent && yarn test && yarn typecheck
```

Expected: all green.

- [ ] **Step 2: Manual sanity check against real leftover files, without a real server**

```bash
mkdir -p /tmp/acltls-qa/tls /tmp/acltls-qa/cfgdir
touch /tmp/acltls-qa/tls/cert.pem /tmp/acltls-qa/tls/key.pem \
  /tmp/acltls-qa/tls.aof /tmp/acltls-qa/tls.snap /tmp/acltls-qa/tls.pid /tmp/acltls-qa/tls-server.log \
  /tmp/acltls-qa/cfgdir/tls-relative.toml /tmp/acltls-qa/acl-tls.toml \
  /tmp/acltls-qa/acltls.aof /tmp/acltls-qa/acltls.snap /tmp/acltls-qa/acltls.pid /tmp/acltls-qa/acltls-server.log \
  /tmp/acltls-qa/acl.aof /tmp/acltls-qa/acl.snap /tmp/acltls-qa/acl.pid /tmp/acltls-qa/acl.toml \
  /tmp/acltls-qa/acl-bad.toml /tmp/acltls-qa/acl-bad2.toml /tmp/acltls-qa/acl-server.log

cd tools/qa-agent && yarn serve &
disown
sleep 1
curl -s -X POST http://127.0.0.1:4848/api/clear-env \
  -H "Content-Type: application/json" -d '{"section":"TLS"}' | node -e '
let data = ""; process.stdin.on("data", d => data += d);
process.stdin.on("end", () => console.log(JSON.parse(data).removedPaths));
'
ls /tmp/acltls-qa
pkill -f "tsx src/index.ts --serve"
rm -rf /tmp/acltls-qa
```

Expected: `removedPaths` lists exactly TLS's 11 entries from Task 1; the `ls` afterward shows only ACL's own files remaining (`acl.aof`, `acl.snap`, `acl.pid`, `acl.toml`, `acl-bad.toml`, `acl-bad2.toml`, `acl-server.log`) — confirming no live TLS/ACL server was needed to prove this, and nothing ACL-owned was touched.

No commit for this task or this plan overall — everything changed lives under gitignored `tools/qa-agent`.

## Next plan

`docs/superpowers/plans/2026-09-15-tls-suite-automation-4-live-verify.md` — actually run the TLS suite through `tools/qa-agent` against a real `rocket-mem` binary (requires the live hand-started cluster to be stopped first — confirm with the user before doing that, it's a shared-system action), capture and fill in `TLS-09`/`TLS-10`'s real output (left as prose in the doc-fixes plan), fix any other real mismatches, and confirm both the "Run" and "Clean environment" buttons genuinely work end to end for TLS in `docs/qa-playbook.html`.
