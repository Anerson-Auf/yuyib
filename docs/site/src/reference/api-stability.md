# Статус API и versioning

> **Статус:** Active policy

- **Stable** — public contract следует semantic versioning policy.
- **Experimental** — API работает, но может измениться в следующем minor release.
- **Planned** — документирует целевое направление и не является доступным API.
- **Research** — compatibility/performance/security contract ещё не принят.
- **Deprecated** — API имеет replacement path и дату удаления.

В раннем foundation новые возможности должны сначала становиться Experimental.
Нельзя помечать importer, renderer path или networking API как Stable до
появления runnable examples, diagnostics, rustdoc coverage и
`Limits & Caveats` documentation. Полный definition of done описан в
[documentation contract](../wiki/documentation-contract.md).
