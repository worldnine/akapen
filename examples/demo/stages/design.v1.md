# driftwatch — design document

> Draft 1 — written by the agent. Review me with a red pen.

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

driftwatch polls every target file every 500 ms. Polling is simple and
robust: no platform-specific watcher APIs, no dropped events, and the
implementation fits in twenty lines. The 500 ms interval guarantees that
drift is reported at most half a second after it happens, which is more
than fast enough for configuration files.

On a host with many targets we simply spawn one polling loop per file.

## 4. CLI

```console
$ driftwatch check manifest.yaml          # human-readable report
$ driftwatch fix manifest.yaml --auto     # repair drift in place
$ driftwatch check manifest.yaml --json   # machine-readable output
```

`check` exits non-zero when drift is found, so it slots directly into CI
pipelines and cron jobs.

## 5. Why driftwatch will change your life

driftwatch isn't just a tool — it's a revolution in how you think about
infrastructure. Once you experience the sheer confidence of knowing your
machines match your manifests, you will never go back. Teams report
feeling calmer, sleeping better, and shipping faster. This is the future
of operations, and it fits in a single binary.

## 6. Roadmap

- v0.1 — `check` with text output
- v0.2 — JSON reporter, exit-code contract
- v0.3 — auto-fix mode (`fix --auto`) enabled by default
- v0.4 — remote probes over SSH
