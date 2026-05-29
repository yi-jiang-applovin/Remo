# Remo Skills

AI agent skills for capability-driven iOS development with Remo.

This repository ships two categories of skills:

- distributed product skills for downstream iOS projects using Remo
- repo-internal contributor skills for working on Remo itself

## Distributed Product Skills

These are the skills intended to be copied into downstream iOS projects.

| Skill | Type | Purpose | When to use |
|-------|------|---------|-------------|
| [`remo-setup`](remo-setup/SKILL.md) | Rigid, one-time | Integrate Remo SDK | First time adding Remo to a project |
| [`remo-capabilities`](remo-capabilities/SKILL.md) | Rigid, periodic | Map features → register capabilities → document | After setup, or when features change |
| [`remo`](remo/SKILL.md) | Flexible, ongoing | Capability-driven development workflow with timeline reports | Every task — drive app state, verify, debug, test, explore |
| [`remo-design-review`](remo-design-review/SKILL.md) | Rigid, periodic | Compare app against Figma designs | Before release, after UI changes, design QA |

### Product Skill Flow

```
remo-setup              One-time: add SDK, wire Remo.start(), verify connection
    ↓
remo-capabilities       Periodic: explore app → register capabilities → write capabilities.md
    ↓
remo                    Ongoing: baseline → [code → build → invoke/checkpoint] → report
    ↓
remo-design-review      Periodic: Figma → state setup → compare captures → compliance report
    ↑                            ↑
    └── feeds back ──────────────┘  (missing capabilities → TODO → implement → re-review)
```

### Product Skill Artifacts

```
.remo/
├── bin/                               # Project-local CLI binary (remo-setup)
│   └── remo
├── capabilities.md                    # Capabilities reference (remo-capabilities)
├── verifications/                     # Verification reports (remo)
│   └── <task-id>/
│       ├── report.md
│       └── assets/
└── design-reviews/                    # Design compliance reports (remo-design-review)
    └── <review-id>/
        ├── report.md
        └── assets/
            ├── figma/                 # Design screenshots from Figma
            └── app/                   # App screenshots from Remo
```

### Install Product Skills

Distributed product skills are installed **per-repo** into downstream iOS
projects at `.claude/skills/`.

```bash
mkdir -p .claude/skills
cp -R /path/to/Remo/skills/remo-setup .claude/skills/
cp -R /path/to/Remo/skills/remo-capabilities .claude/skills/
cp -R /path/to/Remo/skills/remo .claude/skills/
cp -R /path/to/Remo/skills/remo-design-review .claude/skills/
```

## Repo-Internal Contributor Skills

These are for working on the Remo repository itself and should not be copied
into downstream application repos.

| Skill | Purpose | When to use |
|-------|---------|-------------|
| [`example-ios-feature-dev`](example-ios-feature-dev/SKILL.md) | Agentic dev loop for `examples/ios/` — register capabilities, drive & validate with `remo` + XcodeBuildMCP | When building or changing a feature inside `examples/ios/RemoExamplePackage/` |

## CLI Reference

Each distributed product skill folder is self-contained and ships its own
`references/cli.md`.

- Start with [`remo-setup/references/cli.md`](remo-setup/references/cli.md) for the broadest onboarding guide
- That reference now includes both binary installation paths and post-install verification commands
- Use the `references/cli.md` inside the specific distributed product skill you are running for command syntax and caveats that matter to that workflow

## Requirements

- **Remo CLI** — install it either project-locally with `REMO_INSTALL_PREFIX="$PWD/.remo"` so it lands at `.remo/bin/remo`, or globally with `brew install yjmeqt/tap/remo`
- **iOS project** that builds and runs on a simulator
- **Remo SDK** integrated into the app, with app-side Remo code kept under `#if DEBUG` (use `remo-setup` skill if not yet done)
- **Figma MCP server** configured (for `remo-design-review` only)
