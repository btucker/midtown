---
name: midtown_request_review
description: Request another coworker to review your PR. Posts a review request to the channel.
input_schema:
  type: object
  properties:
    pr_number:
      type: string
      description: The pull request number or URL
    reviewer:
      type: string
      description: Optional specific coworker to request review from. If not specified, any available coworker can pick it up.
    description:
      type: string
      description: Brief description of what to focus on in the review
  required:
    - pr_number
---

# Request Review Tool

This tool posts a review request to the team channel, asking another coworker to review your PR.

## When to use

- Your PR is ready for review
- You need a second opinion on implementation choices
- CI has passed and you want human/agent review

## Execution

```bash
# The tool posts a formatted message to the channel
midtown channel post "[REVIEW REQUEST] @{{reviewer}}: Please review PR #{{pr_number}} - {{description}}" --format json
```

If no reviewer specified, the message uses `@team` to indicate any available coworker can pick it up.

## Response handling

The command confirms the message was posted. Another coworker will see the request and can pick it up.

## Best practices

- Include a brief description of what to focus on
- Tag a specific coworker if the PR relates to their area
- Don't request review until CI passes
