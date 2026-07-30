<!-- Thanks for contributing to Pointlock! Please fill out the sections below. -->

## Summary

<!-- What does this PR do, and why? -->

Closes #<!-- issue number, if any -->

## Type of change

- [ ] 🐛 Bug fix
- [ ] ✨ Feature
- [ ] 📖 Documentation
- [ ] ♻️ Refactor / internal
- [ ] 🧪 Tests
- [ ] 🔧 Build / CI / tooling

## Checklist

- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` is clean
- [ ] `cargo fmt --all --check` passes
- [ ] `pnpm -r test` passes (if the UI / TS packages are affected)
- [ ] Generated artifacts are regenerated and committed (schema, `@pointlock/*` types, golden fixtures) — see [CONTRIBUTING](../blob/main/CONTRIBUTING.md#code-generation)
- [ ] Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/)
- [ ] Relevant [design docs](../blob/main/docs/README.md) are updated (for changes to Typed IR / SPI / event vocabulary / projection protocol), or the change is registered in the deviation table

## Notes for reviewers

<!-- Anything reviewers should focus on, trade-offs, follow-ups, etc. -->
