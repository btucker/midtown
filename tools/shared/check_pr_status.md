---
name: midtown_check_pr_status
description: Check the status of pull requests including CI status and review state.
input_schema:
  type: object
  properties:
    pr_number:
      type: string
      description: Optional specific PR number to check. If not provided, lists all open PRs.
  required: []
---

# Check PR Status Tool

This tool checks the status of pull requests from coworkers.

## When to use

- Monitor CI status on open PRs
- Check if reviews have been completed
- See which PRs are ready to merge
- Track overall team PR activity

## Execution

```bash
# List all open PRs
midtown pr list --format json

# Check specific PR (if pr_number provided)
midtown pr list --format json
```

## Response handling

Returns PR information including:
- PR number and title
- Author (coworker name)
- CI status (pending, passing, failing)
- Review status (pending, approved, changes requested)
- Merge readiness

## Best practices

- Check PR status before requesting reviews
- Monitor CI failures to help teammates
- Prioritize approved PRs for merge
