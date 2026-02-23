# Codex Skills

This project includes custom Codex skills for Codex-based coworkers.

## Available Skills

### code-review

Review a GitHub pull request for bugs, CLAUDE.md compliance, and code quality.

**Usage:**
```
/code-review <PR_NUMBER>
```

**What it checks:**
- CLAUDE.md compliance
- Bug scanning (logic errors, missing error handling)
- Git history context
- Previous PR feedback patterns
- Code comment compliance

**Scoring:** Issues are scored 0-100. Only issues scoring 80+ are reported.

## Adding New Skills

Create a directory under `.codex/skills/<skill-name>/` with a `SKILL.md` file:

```
.codex/skills/
└── my-skill/
    └── SKILL.md
```

SKILL.md format:

```markdown
---
name: my-skill
description: |
  What this skill does and when to use it.
  Include trigger phrases users might say.
---

# Skill Title

Instructions for the skill...
```

## Sharing Skills

Skills in `.codex/skills/` are committed to the repository and shared with all team members. Personal skills can be placed in `~/.codex/skills/`.
