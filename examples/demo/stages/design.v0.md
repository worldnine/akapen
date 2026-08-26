# driftwatch — design document

> Draft 0 — outline only.

## 1. Overview

driftwatch detects configuration drift: declared state (YAML) vs the
live system. Read both, diff, report.

## 2. Architecture

- `manifest` — parse declared state
- `probe` — collect live state
- `differ` — diff
- `reporter` — output

## 3. Change detection

TBD — polling? file watchers?

## 4. CLI

TBD.

## 5. Roadmap

TBD.
