# Input Documents

This directory contains raw external inputs that inform requirements and design:

- PM notes and feature requests
- Card docs and specifications from external teams
- Article extracts and research papers
- Prototype references and mockups
- Copied source material from other systems

## Purpose

Raw inputs are kept separate from synthesized requirements to:
1. Preserve original context and intent
2. Allow requirements to evolve from multiple sources
3. Enable traceability from requirement back to source material

## Usage

When creating requirements or design docs, link back to relevant inputs:
```markdown
Source: docs/input/2026-06-20-compaction-research.md
```

Do not treat inputs as implementation-ready specifications. Synthesize them into docs under `docs/requirements/` or `docs/design/` first.
