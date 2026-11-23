# Prepare a DFlow release

Update both READMEs when user-facing behavior changes. For each release, prepare the English and Chinese changelog entries together using the changes since the previous release. The English entry is the full annotated tag message. Add the Chinese entry at the top of [更新日志](../CHANGELOG.zh-CN.md) and commit it before creating the tag.

For a release such as `vX.Y.Z`, start the new section with a version anchor and heading.

```markdown
<a name="release-vX.Y.Z"></a>

## vX.Y.Z

### 改进

- Describe the user-facing change here in Chinese.
```

Use **DFlow** in the English heading and include the GitHub comparison link for follow-up releases. Review the two entries for matching changes. Check the Chinese entry and installation heading locally before committing.

```powershell
pwsh -NoProfile -File .github/release-doc-links.ps1 -Tag vX.Y.Z
```

Keep the version preparation commit limited to `Cargo.toml` and `Cargo.lock`. Confirm the chosen tag matches both. Create an annotated tag from the English Markdown file with `--cleanup=verbatim --file` so Git preserves the headings.

The Windows release workflow checks out the tagged revision, verifies its annotation and docs, and creates or refreshes a draft with the two executable assets. The draft links directly to the changelog entry and installation instructions at that revision. Check the draft contents and links before publishing.
