Work the next ready item on the Ichigo board (user project #4).

Board IDs are in CLAUDE.md. Use them; don't re-query.

1. List project items with Status = Ready (option 61e4505c). If none, stop and
   say so — do not fall back to Backlog.
2. Pick the oldest. If Priority is set, prefer P0 > P1 > P2.
3. Set Status to In progress (47fc9ee4) before touching any code.
4. Read the issue body in full. If acceptance criteria are ambiguous, set Status
   back to Ready, comment on the issue explaining exactly what's underspecified,
   and stop. Do not guess at intent.
5. Create an isolated worktree — do NOT modify this checkout:
     git fetch origin
     git worktree add ../ichigo-<issue-number> -b <type>/<issue-number>-<slug> origin/main
   Then cd into ../ichigo-<issue-number> and do all remaining work there.
   Never run git checkout in the primary checkout.
6. Run the project's install/setup step in the worktree — gitignored files like
   node_modules and .env are not carried over.
7. Implement. Run the full test suite. If tests fail and you can't fix them,
   stop and report — do not open a PR with red tests.
8. Commit, push with -u, open a PR with "Closes #<issue-number>" in the body.
9. Set Status to In review (df73e18b). Report the PR URL and the worktree path.

Never commit to main. Never close the issue. Never set Done.
Do not remove the worktree — leave it for review.
