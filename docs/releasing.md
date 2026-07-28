# Releasing headless-use

Releases are built from a `vX.Y.Z` tag and publish the same Linux x86_64
version to GitHub Releases, npm, crates.io, Docker Hub, and GHCR. The release
workflow is intentionally the only publisher so package versions cannot drift
between channels.

## Distribution architecture

```text
vX.Y.Z tag
    │
    ├── Rust tests + cargo package ──> GitHub tarball + SHA-256
    ├── release binary ──────────────> npm package (binary embedded)
    ├── source package ──────────────> crates.io
    └── Dockerfile ──────────────────> one OCI digest
                                         ├── GHCR
                                         └── Docker Hub
```

GitHub and npm binaries require Chrome or Chromium on the destination host.
The container image includes Chromium and CJK fonts. `serve` and `mcp` remain
stdio services; publishing the image does not turn them into remote HTTP APIs.
The GNU/Linux binary is built on Debian Bookworm so its glibc baseline is not
accidentally raised by changes to the GitHub-hosted runner image.

Distribution contents are intentional per channel:

- GitHub Release: GitHub automatically provides full-repository source archives
  in zip and tar.gz formats. The workflow additionally attaches a minimal
  prebuilt Linux archive containing the executable, `README.md`, and `LICENSE`,
  plus checksums and the npm tarball.
- npm package: executable, npm metadata, npm README, and `LICENSE`.
- crates.io: `src/`, Cargo manifests and lockfile, `build.rs`, `README.md`, and
  `LICENSE`; tests, fixtures, workflows, containers, and maintainer docs are
  excluded.
- Container runtime: the executable, Chromium, fonts, certificates, and `tini`;
  no repository documentation, tests, or source tree is copied into the image.

The release workflow checks the custom binary archive, npm tarball, and crate
file lists before publishing. GitHub owns generation of the full source
archives from the tagged commit. An unexpected file in a custom package fails
the release instead of being silently shipped.

## Required GitHub Actions Secrets

Create a GitHub Environment named `release`, add an approval rule if desired,
and define all external registry values as environment or repository Actions
Secrets. Do not use repository variables for registry identities.

| Secret | Purpose | Minimum access |
| --- | --- | --- |
| `NPM_TOKEN` | Publish `headless-use` to npm | Automation/granular token scoped to the package |
| `CARGO_REGISTRY_TOKEN` | Publish `headless-use` to crates.io | Publish-new-versions permission for the crate |
| `DOCKERHUB_USERNAME` | Select the Docker Hub namespace | Docker Hub account or organization name |
| `DOCKERHUB_TOKEN` | Push the public Docker Hub image | Read/write for `<username>/headless-use` |

GHCR and GitHub Releases use the job-scoped `GITHUB_TOKEN` supplied by GitHub
Actions. It is not a manually managed credential. Artifact attestations use a
short-lived GitHub OIDC identity (`id-token: write`), not a stored secret.

Before the first release, create the public Docker Hub repository and reserve
the `headless-use` names on npm and crates.io. Package names are first-come,
first-served. GitHub Packages creates the GHCR package on the first push; after
that first push, set the GHCR package visibility to public and connect it to
this repository if GitHub did not inherit the repository visibility.

## Version and release procedure

1. Set the same SemVer value in `Cargo.toml` and `npm/package.json`, update
   `Cargo.lock` if Cargo changes it, and update `CHANGELOG.md`.
2. Run the local gates:

   ```bash
   ./scripts/check-release-version.sh
   cargo fmt --all -- --check
   cargo clippy --locked --all-targets --all-features -- -D warnings
   cargo test --locked --workspace -- --test-threads=2
   cargo publish --locked --dry-run
   docker build -f docker/Dockerfile -t headless-use:release-test .
   docker run --rm --security-opt seccomp=unconfined \
     headless-use:release-test \
     run --url https://example.com --screenshot /tmp/smoke.png --no-sandbox
   ```

3. Merge the release commit, then create and push the matching annotated tag:

   ```bash
   git tag -a v1.0.0 -m "headless-use v1.0.0"
   git push origin v1.0.0
   ```

4. Approve the `release` environment deployment. The workflow refuses to
   publish if a required secret is empty or the tag and package versions differ.
5. Confirm all channels report the same version and the two container registries
   report the same digest.

The workflow is safe to rerun after a partial publication. Immutable npm and
crates.io versions are detected and skipped. If only one container registry was
updated, the existing manifest is copied to the missing registry instead of
rebuilding the version tag. If both registries contain different digests, the
workflow stops for manual investigation rather than overwriting either image.
Resume an existing tag from the Actions page with **Run workflow** and enter the
original `vX.Y.Z` tag. The workflow checks out that immutable tag while using the
current workflow definition, so publishing can recover from an automation fix
without moving the release tag or rebuilding an already published image.

## Consumer verification

GitHub Release includes `checksums.txt` for the tarball and npm archive. Verify
build provenance with GitHub CLI:

```bash
sha256sum --check checksums.txt
gh attestation verify headless-use-v1.0.0-x86_64-unknown-linux-gnu.tar.gz \
  --repo flyingsquirrel0419/headless-use
gh attestation verify \
  oci://ghcr.io/flyingsquirrel0419/headless-use:1.0.0 \
  --repo flyingsquirrel0419/headless-use
```

Use a full SemVer image tag or an immutable digest for deployments. `latest`
is a convenience tag and moves whenever a new stable release is published.

## Decision log

`[Decision Log]`

- Intent: Make every supported installation path available from one release.
- Previous implementation and constraints: Only a GitHub Linux tarball was
  produced; stdio execution and the external-browser requirement must remain.
- Alternatives considered: Separate registry workflows, install-time npm
  downloads, GHCR-only images, and independently rebuilt registry images.
- Chosen approach: A tag-gated workflow, embedded npm binary, source crate, and
  one attested OCI digest mirrored to GHCR and Docker Hub.
- Why this approach over the alternatives: It prevents version drift, avoids
  npm install scripts, and gives both GitHub-native provenance and Docker Hub
  discoverability.
- Pros, cons, and impact: Consumers gain consistent packages and verifiable
  provenance; maintainers must manage four Actions Secrets and wait for the
  complete test matrix before publishing.
