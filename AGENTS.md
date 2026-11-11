## Development Workflow

**IMPORTANT**: After any code change, bug fix, or feature addition/removal, you MUST complete all of these steps:

1. **Update README.md** if the change affects:
   - Usage examples or commands
   - Installation instructions
   - Configuration options
   - Available features

2. **Update CHANGELOG.md**:
   - Add new version number following semantic versioning (MAJOR.MINOR.PATCH)
   - Add entry under appropriate category (Added, Changed, Fixed, Removed)
   - Include date in format YYYY-MM-DD

3. **Commit and push changes**:
   - Use `git add` to stage all modified files (README.md, CHANGELOG.md, and code files)
   - Create descriptive commit message following existing style
   - Push to remote repository with `git push`

4. **Release**: when the version number changed, tag the commit and push the tag to publish a GitHub Release (the in-app updater picks it up):
   - `git tag vX.Y.Z` (must match `version` in `Cargo.toml`)
   - `git push origin vX.Y.Z`
   - Wait for the CI run on `master` for that commit to finish before pushing the tag, so the release reuses its build cache (a cold release build takes 30+ minutes).

5. **Clean up**: after every push, run `cargo clean` to free the disk space used by `target/`.