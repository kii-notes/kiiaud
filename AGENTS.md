# Agent Guidelines

## Build & Release Policy (GitHub Actions)

- **Triggering builds:** Always use `workflow_dispatch` on [`.github/workflows/build.yml`](.github/workflows/build.yml) via `gh workflow run` or the GitHub REST API.
  - Specify the target branch/ref (`--ref <branch>`) and `target` input (`all`, `linux-x86_64`, `linux-arm64`, `windows-x86_64`, `android-arm64`, or `android-armv7`), e.g.:
    ```bash
    gh workflow run build.yml --ref <branch> -f target=all
    ```
  - Monitor the triggered run (`gh run list` / `gh run watch`) and report the result and commit SHA.
- **No tag/commit workarounds:**
  - Never create, push, move, or delete Git tags to trigger a build.
  - Never create empty or dummy commits merely to trigger a build.
  - Do not add automatic full-platform builds to `push` or `pull_request` triggers unless explicitly requested.
- **Artifacts vs. Releases:**
  - Builds upload to GitHub Actions artifacts with platform, architecture, commit SHA, and run ID metadata (`build-metadata.json`).
  - Never create GitHub Releases or `v*` version tags during build operations.
- **Official releases:**
  - Git tags (`v*`) are reserved exclusively for intentional versioning and official releases via [`.github/workflows/release.yml`](.github/workflows/release.yml).
  - Only create a `v*` tag when explicitly instructed to release a version.
