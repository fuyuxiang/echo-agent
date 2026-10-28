---
name: github-ops
description: "GitHub CLI for issues, PRs, code search, CI logs, releases, and API queries. Requires gh CLI and auth."
version: 1.0.0
metadata:
  echo:
    tags: [GitHub, PR, Issues, CI, DevOps, Git]
    requires:
      bins: [gh]
---

# GitHub Ops

Use `gh` for GitHub operations. Use `git` for local commits/branches/push/pull.

## Auth

```bash
gh auth status
gh auth login
```

## Pull Requests

```bash
gh pr list --repo owner/repo --json number,title,state,author,url
gh pr view 55 --repo owner/repo --json title,body,author,files,commits,reviews
gh pr checks 55 --repo owner/repo
gh pr diff 55 --repo owner/repo
gh pr create --repo owner/repo --title "feat: title" --body "description"
gh pr merge 55 --repo owner/repo --squash
```

## Issues

```bash
gh issue list --repo owner/repo --state open --json number,title,labels,url
gh issue view 42 --repo owner/repo --json title,body,comments,labels
gh issue create --repo owner/repo --title "Bug: ..." --body "details"
gh issue comment 42 --repo owner/repo --body "Fixed in abc123"
gh issue close 42 --repo owner/repo
```

## CI / Actions

```bash
gh run list --repo owner/repo --limit 10
gh run view <run-id> --repo owner/repo --json status,conclusion
gh run view <run-id> --repo owner/repo --log-failed
gh run rerun <run-id> --repo owner/repo --failed
```

## Code Search

```bash
gh search code "pattern" --repo owner/repo
gh search repos "echo-agent" --language python
```

## Releases

```bash
gh release list --repo owner/repo
gh release create v1.0.0 --repo owner/repo --title "v1.0.0" --notes "Release notes"
gh release download v1.0.0 --repo owner/repo
```

## API (advanced)

```bash
gh api repos/owner/repo --jq '.stargazers_count'
gh api repos/owner/repo/pulls --jq '.[].title'
gh api notifications --jq '.[].subject.title'
gh api user/repos --jq '.[].full_name' --paginate
```

## Notifications

```bash
gh api notifications --jq '.[] | "\(.subject.type): \(.subject.title)"'
```
