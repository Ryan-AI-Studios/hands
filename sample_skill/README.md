# Sample harness skill

Copy **only** `helping-hands/SKILL.md` into your agent’s skills directory if you want
the model to **drive** Hands (observe / click / fusion debug). Install itself is
the repo-root `README.md`.

Examples (pick what your harness uses):

```text
<this-clone>\.agents\skills\helping-hands\SKILL.md
%USERPROFILE%\.grok\skills\helping-hands\SKILL.md
```

Do **not** copy this repo’s gitignored `.agents/skills/implement`, `onboarding`,
`ledgerful`, or `ai-brains` skills. Those assume the owner planning tree one
level up from this clone (`conductor/`, ADRs) and are not required to run Hands.
