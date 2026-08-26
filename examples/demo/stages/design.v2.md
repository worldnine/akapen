# driftwatch — design document

> Draft 2 — revised after review round 1.

## 1. Overview

**driftwatch** is a small CLI that detects *configuration drift*: the gap
between the state you declared (a YAML manifest) and the state a live
machine is actually in. It reads both, diffs them, and reports.

driftwatch never modifies the system. It is strictly read-only — a
reporter, not an enforcer.

## 2. Architecture

| Component  | Responsibility                                  |
| ---------- | ----------------------------------------------- |
| `manifest` | parse the declared state from YAML              |
| `probe`    | collect the live state (files, services, ports) |
| `differ`   | structural diff between declared and live       |
| `reporter` | render the diff as text, JSON, or exit code     |

Each component is a separate module with no shared mutable state; the
pipeline is `manifest → probe → differ → reporter`, one direction only.

## 3. Change detection

driftwatch is event-driven: it subscribes to the platform file watcher
(FSEvents on macOS, inotify on Linux) and re-probes only the targets
that actually changed. A single watcher covers all targets, so cost does
not grow with the number of files.

Where no watcher API is available, driftwatch falls back to polling at a
configurable interval (default 5 s) — a fallback, not the design.

## 4. CLI

```console
$ driftwatch check manifest.yaml          # human-readable report
$ driftwatch check manifest.yaml --json   # machine-readable output
$ driftwatch watch manifest.yaml          # stay running, report drift live
```

`check` exits non-zero when drift is found, so it slots directly into CI
pipelines and cron jobs.

## 6. Roadmap

- v0.1 — `check` with text output
- v0.2 — JSON reporter, exit-code contract
- v0.3 — auto-fix mode (`fix --auto`) enabled by default
- v0.4 — remote probes over SSH
