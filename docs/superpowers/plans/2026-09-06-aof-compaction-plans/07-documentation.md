# BGREWRITEAOF Documentation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `BGREWRITEAOF` appears in the two places every other command already does — README's command-coverage table and `docs/command-compatibility.md`.

**Architecture:** two one-line edits, following the exact existing format of both tables.

**Tech Stack:** none.

**Spec:** [`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md), "Definition of done".

## Global Constraints

- Depends on plans 01–06 all being merged first — this documents finished, tested behavior, not a plan.
- No behavioral/code changes in this plan.

---

### Task 1: README and command-compatibility.md

**Files:**
- Modify: `README.md`
- Modify: `docs/command-compatibility.md`

**Interfaces:**
- Consumes: nothing — pure documentation.
- Produces: nothing — this is the last plan in this feature.

- [ ] **Step 1: Update README's command-coverage table**

In `README.md`'s `## Command coverage` section, the `Server/Cluster` row currently reads:

```
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
```

Change it to add `BGREWRITEAOF` directly after `` `SAVE` ``:

```
| Server/Cluster | `PING`, `ECHO`, `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `BGREWRITEAOF`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
```

- [ ] **Step 2: Update `docs/command-compatibility.md`'s command coverage table**

In `docs/command-compatibility.md`'s `## Command coverage` section, the `Server/Cluster` row (`docs/command-compatibility.md:23`) currently reads:

```
| Server/Cluster | `PING`, `ECHO`, `DEBUG SLEEP`[^debug-sleep-cap], `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
```

Change it the same way:

```
| Server/Cluster | `PING`, `ECHO`, `DEBUG SLEEP`[^debug-sleep-cap], `SELECT`, `COMMAND`, `HELLO`, `INFO [section]`, `SAVE`, `BGREWRITEAOF`, `REPLICAOF`, `PSYNC`, `CLUSTER KEYSLOT`/`SHARDS`/`NODES`/`INFO`/`MYID`, `SLOWLOG GET`/`LEN`/`RESET` |
```

- [ ] **Step 3: Verify**

Run: `grep -n "BGREWRITEAOF" README.md docs/command-compatibility.md`
Expected: one match in each file, in the `Server/Cluster` row.

- [ ] **Step 4: Commit**

Use the `1-git-commit` skill to commit `README.md` and `docs/command-compatibility.md`.

---

## Next plan

None — this is the last plan for the AOF compaction feature. Once it's merged, every checklist item in the design spec's "Definition of done" ([`../../specs/2026-09-06-aof-compaction-design.md`](../../specs/2026-09-06-aof-compaction-design.md)) should be checked off; do a final pass over that list to confirm before considering the feature complete.
