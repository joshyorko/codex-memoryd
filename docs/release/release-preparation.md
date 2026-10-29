# MemoryD PID-safety release preparation

Prepared in an isolated cloud workspace on 2026-10-29. This is a plan, not a release or deployment authorization. It does not authorize any command against Dakota, a live MemoryD instance, Kamal, or the #58 recovery campaign. This document is a point-in-time handoff: recheck all state before a release.

## Current handoff

| Field | Value |
| --- | --- |
| `DEPLOYMENT_READY` | **BLOCKED / NOT_READY** |
| Merged PR | https://github.com/joshyorko/codex-memoryd/pull/275; issue #269 closed |
| Merged source commit | `fea268928b197b09ca602a73e8f26bf867fd2e9e` on `master` |
| Independently reviewed and locally tested PR head | `752fc47617fc73101497ddf42616208f80d0f586`, second parent of the merge commit |
| Post-merge source CI | Linux release gate and lifecycle smoke, plus macOS fail-closed test, **passed**: https://github.com/joshyorko/codex-memoryd/actions/runs/40918116096 |
| Proposed next stable version | `v0.1.1` is a proposal only. `v0.1.0` is the sole current stable release and sole existing tag. Decide whether the macOS managed-lifecycle incompatibility merits another versioning choice before preparing a release commit. |
| Release tag and release source commit | **Pending**: no new tag or release exists. A future version/workflow commit must include the merge above; record its own exact SHA after validation. |
| Native binary archive checksums | **Pending** actual published artifacts and verification. |
| Homebrew version and artifact checksums | **Pending** decision and publication of a verified formula. The currently published master-tracking formula is `20261014222239.79a279d5f730`, from pre-fix commit `79a279d5f730162ad34be9b6410e666ba878a05c`; it does not include #275. |
| GHCR image and manifest digest | **Pending** actual published image and verification. |
| Release provenance and reproducibility | **Pending** release-specific evidence and an independent rebuild comparison; the current source workflow provides unsigned archive metadata, not a signed attestation. |
| Published / deployed | No new stable release, tag, Homebrew update, GHCR image, or deployment was created by this worker. |

The existing v0.1.0 native binary and the currently published Homebrew build predate the fix. Never use their native `down` or `restart` based on a PID file. Do not assume a new manager can manage the old process or its legacy PID file.

## Decisions for the operator

Recommended for the urgent safety fix: publish a patch release with the existing fail-closed macOS behavior, explicitly warning that managed native `up`, `down`, and `restart` no longer work there. macOS users can run foreground `serve` under a trusted supervisor or use a container. An alternative is to delay the stable release until race-safe macOS management is implemented and independently reviewed. Josh decides between those options and the final version number; `v0.1.1` is only a proposal.

Once the release source, publisher, tap safeguard, CI, reproducibility, provenance, and outstanding operator checks are ready for review, request explicit approval for the **exact version and source commit**, including the planned GitHub release, Homebrew update, GHCR version tag, and any movement of `latest`. Approval to publish does not authorize deployment. This preparation does not create or approve any of those artifacts.

## What must be prepared and reviewed before release

1. Agree on the version and platform compatibility. The proposed fix supports race-resistant managed native lifecycle on Linux when pidfds, readable `/proc`, and required permissions are available. On macOS and other non-Linux platforms, managed native `up`, `down`, and `restart` fail closed; foreground `serve` under a suitable supervisor and container mode remain alternatives. This is a material change from v0.1.0. Josh can choose to release with that documented limitation, or defer the stable release until race-safe managed macOS lifecycle support is implemented and tested. This preparation does not make that choice. `CODEX_MEMORYD_BIN` must resolve to the manager's same executable inode; symlinks and hard links are supported, wrappers and different binary builds are not.
2. Prepare and review a version-specific source change, including `Cargo.toml`, the root package entry in `Cargo.lock`, version checks and documentation. `.github/workflows/release-v0.1.0.yml` is explicitly locked to the old tag, version, checkout refs, and smoke-test version. A future tag does **not** trigger that publisher as written. The separate `.github/workflows/release-preflight.yml` derives its candidate version from `Cargo.toml`, runs Linux managed-lifecycle and macOS fail-closed/foreground checks, and compares independent native and unpublished OCI builds. It only handles pull requests and manual dispatch, has read-only repository permission, and does not publish a release, package, or container. Adapt or create a reviewed publisher for the selected version while preserving exact-tag checks, immutable release checks, asset validation, and fail-closed publication. Until that is done, a new stable release is not ready to publish. Do not create a tag during preparation.
3. Coordinate Homebrew as its own release channel. The actual tap is https://github.com/joshyorko/homebrew-tools. `Formula/codex-memoryd.rb` currently uses timestamp-plus-commit versions and tap-hosted assets; it does not automatically use the stable GitHub release archive or its semver. The tap's `codex-memoryd-update.yml` rebuilds latest master on four platforms, then publishes a tap release and commits the formula, if all jobs pass. Its macOS lifecycle smoke, and the formula's own macOS `brew test`, currently call managed `up` and `down` and will fail with this fix. Decide whether to make the primary formula consume the verified stable release, or retain a clearly separate master-tracking channel. In either case update and review the publisher and tests, then verify every formula URL and checksum against the intended artifact provenance. The currently observed tap-hosted release has `immutable: false`; do not describe that asset set as immutable without fixing or independently establishing that property.
4. The tap has a daily scheduled `codex-memoryd-daily` job at **10:37 UTC**, which attempts to rebuild latest master and can publish without a new MemoryD tag. The current macOS smoke should stop this particular commit before publication, but that failure must not be treated as an authorization gate or a guarantee about future workflow changes. A narrowly scoped proposed safeguard is to gate the tap's `publish` job on a nonempty operator-approved source SHA that equals `needs.resolve.outputs.commit`; retain the shared schedule and other packages. Implementing that tap change or setting its approval variable requires separate authorization. No tap workflow or settings were changed here.
5. Establish artifact evidence before publication. The old native workflow pins Rust 1.88.0 and locked dependencies, normalizes archive metadata, records per-platform SHA-256 and source commit in `provenance.json`, stages and checks four archives, requires an immutable GitHub release, and builds linux/amd64 plus linux/arm64 GHCR images from the same source commit with OCI revision labels. Metadata and labels are assertions, not signed provenance. The read-only preflight records unsigned, seven-day CI candidate artifacts and compares two separate runner builds for each native target and each Linux OCI platform. It sets a source timestamp and normalizes exported layers; it does not push an image. Its OCI comparison intentionally excludes embedded attestations and SBOMs from the compared image, while retaining full unsigned Buildx metadata separately. Neither the candidate digest nor metadata is a published artifact identity or a signed attestation. The old publication workflow sets `provenance: false` and `sbom: false`; the Dockerfile uses pinned base digests but a floating Dockerfile frontend and mutable `apt-get update`. Runner images, Buildx, and BuildKit may also change. Before publication, review a publisher that uses the verified build recipe, adds independently verifiable attestations, pins or snapshots OS dependencies, and verifies actual artifact subjects and per-platform image content. If any rebuild comparison fails or evidence is unavailable, reproducibility is unproven.
6. Run the focused checks and full synthetic gate below on the final release source, obtain the required independent/repository reviews, and make sure all final-head CI passes. The existing repository documentation additionally marks live dogfood checks as tag-required; those checks were **not run** here. Its older operator instructions include service and Compose lifecycle actions, so they must be reviewed for PID safety and replaced or separately authorized by the deployment coordinator where appropriate. Do not run those live checks from this cloud worker.
7. Only after explicit release authorization, create the approved tag and publish through the reviewed workflow. Record the actual tag, exact release commit, immutable-release status, checksums, tap version, GHCR manifest digest, and provenance links. Only the separately authorized deployment coordinator may decide and execute a live rollout.

## Validation commands to run later in disposable environments

These commands are a release checklist, not evidence of a future run. Linux lifecycle tests must run in an isolated cloud worker or VM with no production processes, data, socket, or runtime directory. They spawn only disposable fixtures. Do not run them on Dakota or against a live runtime.

```bash
# On the final release-source checkout, before tagging:
git rev-parse HEAD
git merge-base --is-ancestor fea268928b197b09ca602a73e8f26bf867fd2e9e HEAD
python3 scripts/run-with-child-reaper.py cargo test --locked --test native_pid_safety
python3 scripts/run-with-child-reaper.py cargo test --locked --lib native_runtime::native_process::platform::tests
python3 scripts/run-with-child-reaper.py cargo test --locked --test native_instance_isolation
check_dir="$(mktemp -d)"
TMPDIR="$check_dir" python3 scripts/run-with-child-reaper.py \
  scripts/v0.1-release-gate.sh --artifact-dir "$check_dir/artifacts"
jq -e '.status == "pass" and .operator_dogfood_included == false' \
  "$check_dir/artifacts/release-gate-summary.json"
```

Expected: ancestry check and every test/gate exit zero, and the summary expression outputs `true`; record `git rev-parse HEAD` as the **future** tested release commit. The skipped live lane remains a separate release decision. On a disposable macOS runner, `cargo test --locked --test native_unsupported_safety` must pass and its disposable child remain alive; exercise foreground `serve` with a supervisor and temporary database, without relying on managed native `up`/`down`.

After an authorized publication, with `tag`, `version`, and `release_commit` set to the actual recorded values, use a fresh clone and temporary download directory:

```bash
git fetch origin "refs/tags/$tag:refs/tags/$tag"
test "$(git rev-parse "$tag^{commit}")" = "$release_commit"
git merge-base --is-ancestor fea268928b197b09ca602a73e8f26bf867fd2e9e "$release_commit"
gh api "repos/joshyorko/codex-memoryd/releases/tags/$tag" |
  jq -e --arg tag "$tag" '.tag_name == $tag and .draft == false and .prerelease == false and .immutable == true'
dist="$(mktemp -d)"
gh release download "$tag" --repo joshyorko/codex-memoryd --dir "$dist"
(cd "$dist" && sha256sum -c SHA256SUMS)
jq -e --arg tag "$tag" --arg version "$version" --arg commit "$release_commit" \
  '.repository == "joshyorko/codex-memoryd" and .ref == ("refs/tags/" + $tag) and .package_version == $version and .commit == $commit' \
  "$dist/provenance.json"
```

Expected: both Git commands exit zero, the release query and provenance query output `true`, and all four listed archive checksums print `OK`. Compare `provenance.json` artifact names, sizes and hashes to those downloads and the independently reproduced builds; inspect the release workflow run and any verified attestations for the same commit and artifacts. An unsigned JSON record alone is insufficient evidence of an independently verified builder. Run the Linux lifecycle smoke and macOS fail-closed/foreground smoke against the **downloaded** binaries in throwaway runners.

For the actual published Homebrew version and image digest, set `brew_version` and `image_digest` from verified published evidence, then in disposable hosts only:

```bash
brew update
brew info --json=v2 joshyorko/tools/codex-memoryd |
  jq -e --arg version "$brew_version" '.formulae[0].versions.stable == $version'
brew install joshyorko/tools/codex-memoryd
brew test joshyorko/tools/codex-memoryd
codex-memoryd --version

image=ghcr.io/joshyorko/codex-memoryd
actual_digest="$(docker buildx imagetools inspect "$image:$tag" --format '{{json .Manifest}}' | jq -er '.digest')"
test "$actual_digest" = "$image_digest"
docker buildx imagetools inspect "$image:$tag" --raw |
  jq -e '[.manifests[] | select(.platform.os == "linux") | .platform.architecture] | sort == ["amd64", "arm64"]'
docker pull --platform linux/amd64 "$image@$image_digest"
docker image inspect "$image@$image_digest" --format '{{index .Config.Labels "org.opencontainers.image.revision"}}'
```

Expected: Homebrew query returns `true`, install and formula tests exit zero, and CLI version is the selected package version. The digest comparison exits zero, the platform check outputs `true`, and image revision equals `release_commit`; independently verify the image's attestations and run an isolated loopback `/healthz` smoke with a temporary database. Repeat platform-specific smoke on both Linux architectures; compare published version and `latest` only if moving `latest` was explicitly authorized. If Homebrew retains independently built tap archives, its checksums and provenance must be checked against those actual tap assets as well.

## Migration, rollback, and container scope

- Before an upgrade, the coordinator should make a verified backup using the existing approved process and determine which supervisor owns the old daemon. Stop it through that trusted supervisor or terminal; do not trust the numeric PID file, a process name, or the vulnerable v0.1.0 native `down`/`restart`. If ownership cannot be safely established, leave the runtime untouched and escalate.
- Once the old daemon is confirmed stopped and the endpoint unused, remove only its stale PID file and adjacent `.identity` file in the selected runtime. Preserve the database, configuration, backups, and unrelated processes. Start the new Linux manager, check status and health, and verify the original persistent-memory data. A live legacy PID or malformed identity will otherwise be reported unverified and managed actions refuse to proceed.
- For rollback, retain a verified prior artifact and database backup. Stop the new managed process with the new verified manager or its original trusted supervisor. Confirm storage compatibility before restoring a prior binary or image. A v0.1.0 rollback reintroduces the PID vulnerability: do not use its native PID-file lifecycle; use a safely supervised foreground service if rollback is essential. For images, use the previously verified digest supplied by the coordinator; it is **pending** here. Do not infer it from a mutable tag.
- The checked-in Dockerfile starts `codex-memoryd serve` in the foreground; the fixed code concerns managed native PID-file lifecycle. **If** `kamal-test-deploy` runs only that foreground container path and has no host-native lifecycle use, this specific bug does not require a Kamal deployment. This is an inference from source, not an inspection of that environment. The deployment coordinator must verify the actual arrangement before deciding; this worker has not accessed it.

## Review and remaining test limits

Sol's independent adversarial review initially found descendant startup acknowledgment and FIFO blocking defects; both were fixed and independently re-tested at `752fc47617fc73101497ddf42616208f80d0f586`, with no unresolved safety findings in the reviewed scope. The exact head also received completed Codex connector reviews: https://github.com/joshyorko/codex-memoryd/pull/275#issuecomment-6503050549 and https://github.com/joshyorko/codex-memoryd/pull/275#issuecomment-6503080903. Focused checks and the full synthetic release gate passed; the gate's live dogfood lane was skipped. Actual kernel PID recycling, old kernels, restricted procfs, and crash injection were not exercised; PID reuse was modeled deterministically. Same-user access to runtime files is not a security boundary. Future release-specific code and artifacts require their own final-head review and validation.

Issues [#270](https://github.com/joshyorko/codex-memoryd/issues/270) and [#271](https://github.com/joshyorko/codex-memoryd/issues/271) remain open and separate from PR #275.
