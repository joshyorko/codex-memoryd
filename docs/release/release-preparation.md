# MemoryD PID-safety release preparation

Prepared in an isolated cloud workspace on 2026-10-29. This is a plan, not a release or deployment authorization. It does not authorize any command against Dakota, a live MemoryD instance, Kamal, or the #58 recovery campaign. This document is a point-in-time handoff: recheck all state before a release.

## Current handoff

| Field | Value |
| --- | --- |
| `DEPLOYMENT_READY` | **BLOCKED / NOT_READY** |
| `PRE_RELEASE_ACCEPTANCE` | **PENDING** final-source operator evidence; it does not authorize deployment. |
| Merged PR | https://github.com/joshyorko/codex-memoryd/pull/275; issue #269 closed |
| Merged source commit | `fea268928b197b09ca602a73e8f26bf867fd2e9e` on `master` |
| Independently reviewed and locally tested PR head | `752fc47617fc73101497ddf42616208f80d0f586`, second parent of the merge commit |
| Post-merge source CI | Linux release gate and lifecycle smoke, plus macOS fail-closed test, **passed**: https://github.com/joshyorko/codex-memoryd/actions/runs/40918116096 |
| Next stable version | `v0.1.1` publication is conditionally approved after all acceptance, review, and provenance gates pass. It has not been tagged or published. `v0.1.0` is the sole current stable release and existing tag. |
| Release tag and release source commit | **Pending**: no new tag or release exists. A future version/workflow commit must include the merge above; record its own exact SHA after validation. |
| Native binary archive checksums | **Pending** actual published artifacts and verification. |
| Homebrew version and artifact checksums | **Pending** publication of a verified formula. The currently published master-tracking formula is `20261014222239.79a279d5f730`, from pre-fix commit `79a279d5f730162ad34be9b6410e666ba878a05c`; it does not include #275. |
| GHCR image and manifest digest | **Pending** actual published image and verification. |
| Release provenance and reproducibility | **Pending** a future authorized publisher run on the exact release tag, independent rebuild comparison, signed attestations, and verification. CI candidate evidence is not release provenance. |
| Published / deployed | No new stable release, tag, Homebrew update, GHCR image, or deployment was created by this worker. |

The existing v0.1.0 native binary and the currently published Homebrew build predate the fix. Never use their native `down` or `restart` based on a PID file. Do not assume a new manager can manage the old process or its legacy PID file.

## Decisions for the operator

Josh has accepted the fail-closed macOS behavior for this release: managed native `up`, `down`, and `restart` do not operate; foreground `serve` under a trusted supervisor and container mode remain supported. Native status reports the process as unverified. Publication of `v0.1.1` is approved only when the required acceptance, review, and provenance gates pass. Deployment remains separate.

The release source SHA is pending until the changes, exact-head reviews, and CI are final. Outstanding evidence includes source-tied operator acceptance, final-source rebuild comparisons, and verified published artifacts. Do not create credentials or expand access under the publication approval. The tap guard also needs its own review and tests. No approval covers deployment or moving GHCR `latest`.

## What must be prepared and reviewed before release

1. Confirm the release version. The fix supports race-resistant managed native lifecycle on Linux when pidfds, readable `/proc`, and required permissions are available. Josh accepted fail-closed managed native `up`, `down`, and `restart` on macOS and other non-Linux platforms; foreground `serve` under a suitable supervisor and container mode remain alternatives. This is a material change from v0.1.0. `CODEX_MEMORYD_BIN` must resolve to the manager's same executable inode; symlinks and hard links are supported, wrappers and different binary builds are not.
2. Review the proposed version-specific source change, including `Cargo.toml`, the root package entry in `Cargo.lock`, version checks and documentation. `.github/workflows/release-v0.1.0.yml` is explicitly locked to the old tag; a future tag does **not** trigger it. The new `.github/workflows/publish-release.yml` is a version-neutral, manual-only publisher. It requires the selected tag, the explicitly supplied full commit SHA, the package version, and master ancestry to match; a tag or branch push does not invoke it. It calls the read-only `.github/workflows/release-preflight.yml` on that exact source, requires the full synthetic gate, verifies same-run candidate pairs, signs and verifies native subjects, stages and checks a draft release, then publishes and verifies the immutable release before pushing either OCI platform. It rebuilds and checks both OCI platform digests before assigning the versioned image and signing and verifying its index. It also checks anonymous image availability and runs an isolated foreground health smoke. The draft is looked up through the authenticated release list because GitHub's release-by-tag endpoint returns published releases. The workflow does not assign GHCR `latest`. This workflow is preparation code and has not been run through actual publication or signed an artifact.
   The publisher uses the built-in workflow token and GitHub OIDC for attestations; no new administration credential is required. As in the v0.1.0 workflow, it checks the published release's `immutable: true` result and verifies the published assets. It does not query the repository's immutability setting before publication. If the setting has been disabled, the release can become public and mutable before the check fails; GHCR and Homebrew publication must not continue. An administrator may independently check the setting before publication, but that is not an automated pre-publication guarantee. Inspect any partial release before retrying. A current GitHub CLI with attestation and immutable-release verification commands is required and checked before writes.
3. Coordinate Homebrew as its own release channel. The actual tap is https://github.com/joshyorko/homebrew-tools. The current formula uses timestamp-plus-commit versions and tap-hosted assets; it does not automatically use the stable GitHub release archive or its semver. A [proposed tap branch](https://github.com/joshyorko/homebrew-tools/tree/prepare/codex-memoryd-pid-safety) gates updates on an immutable upstream release for the exact master commit, verifies its signed attestations, compares independently built binaries, and reuses the verified upstream archive bytes. Its smoke tests use a disposable foreground child. The branch is not merged or published; review and test it before use, then verify each formula URL and checksum against the actual released artifacts. The currently observed tap-hosted release has `immutable: false`; do not describe that asset set as immutable.
4. The tap has a daily scheduled `codex-memoryd-daily` job at **10:37 UTC**, which currently attempts to rebuild latest master and can publish without a new MemoryD tag. The proposed guard requires exactly one stable, published immutable upstream release whose tag resolves to that master commit before building; publication again verifies the release, source, attestations, and matching binaries. An unset `CODEX_MEMORYD_APPROVED_SOURCE_SHA` does not block an eligible release; if the variable is already set, both build and publish require the commit to match that optional pin. Do not overwrite an existing pin. The schedule and other packages remain unchanged. Have the reviewed tap guard in place before merging this release-source PR to master. No tap settings or default-branch workflows have been changed.
5. Establish artifact evidence before publication. The read-only preflight records unsigned, seven-day CI candidate artifacts and compares two separate runner builds for each native target and each Linux OCI platform. It then runs the publisher's candidate guards on those same-run artifacts without producing a release bundle. It compares the OCI exporter with the publisher's non-pushing image exporter, sets a source timestamp, and normalizes exported layers; it does not push an image. Its OCI comparison excludes embedded attestations and SBOMs while retaining unsigned Buildx metadata separately. Neither the candidate digest nor metadata is a published artifact identity or signed attestation. The prepared publisher creates separate GitHub provenance attestations for the actual native files and OCI index, and verifies them against the tag, commit, and publisher workflow. Those operations have not been tested against the real attestation service. The Dockerfile still uses a floating frontend and mutable `apt-get update`; runner images, Buildx, and BuildKit may change. The publisher requires matching build tools and fails if the final local or pushed platform digest differs from the two candidates. This detects drift for that run; it does not make upstream repositories or the registry immutable. If any comparison or attestation check fails, reproducibility or provenance remains unproven.
   Independent preflight builds found different OCI manifest digests on both Linux architectures: https://github.com/joshyorko/codex-memoryd/actions/runs/40922940280. Layer diagnostics isolated the differences to timestamped apt/dpkg logs and the linker auxiliary cache produced during package installation. After those build-only files were removed, independent OCI comparisons and disposable container health checks passed for both architectures at `625a35df29c51a82d27c276642b880df96e8cd57`: https://github.com/joshyorko/codex-memoryd/actions/runs/40924143300. Recheck the final release source; the preflight retains file-hash diagnostics, not temporary OCI image archives. Rerun the whole workflow rather than only failed matrix jobs so both build slots share one run attempt.
   Prior macOS ARM candidate comparisons found identical binaries but different compressed archives: https://github.com/joshyorko/codex-memoryd/actions/runs/40922940280. A later diagnostic identified differing Python versions; the preflight now pins Python 3.14.10 and records Python/zlib versions and uncompressed tar hashes. Subsequent runs produced different macOS ARM binary hashes despite matching recorded tools and sizes: https://github.com/joshyorko/codex-memoryd/actions/runs/40933148026 and https://github.com/joshyorko/codex-memoryd/actions/runs/40934099736. The first failure used the same reported macOS and runner image versions in both jobs; the second used different versions. Their cause is unresolved. The preflight now logs compiler, OS, and SDK details, and the comparison reports Mach-O load-command, section, and code-signature differences on failure; neither relaxes the byte-for-byte requirement. A fresh comparison must pass on the exact final release source; the earlier mismatches remain a risk if they recur.

   A separate Linux rebuild from two checkout and target directories embedded the different source paths in the binaries and failed the tap's byte-for-byte comparison. Both the upstream candidate workflow and proposed tap build source `scripts/release-native-build-env.sh`, which remaps the source and Cargo home paths to stable prefixes before compiling. Experimental builds of the `1fdb4642109622f9c7d23344b02ca2cee19ad42c` source with these flags matched across different checkout, Cargo home, and target paths containing spaces and passed an isolated lifecycle smoke. They are not release artifacts. Recheck independent builds on every final target; these flags do not explain or waive the earlier macOS mismatch.
6. Run the focused checks and full synthetic gate below on the final release source, obtain the required independent/repository reviews, and make sure all final-head CI passes. The existing repository documentation additionally marks live dogfood checks as tag-required; those checks were **not run** here. The operator lane starts and rebuilds Compose, applies `sync-local` to `.dogfood/memory.db`, copies that DB to sandboxes, exercises native lifecycle and writes sandbox canaries. The checked-in Compose file fixes the container name `codex-memoryd`, image `codex-memoryd:latest`, and loopback port `8787`, mounts the checkout's `.dogfood` and host memories, and enables automatic Dreamer apply. Changing only `DOGFOOD_COMPOSE_PROJECT` does not isolate those resources. The deployment coordinator must establish a safely isolated and authorized environment or provide valid, operator-approved acceptance evidence without disturbing existing services or databases. The heartbeat and its subsequent shutdown now use the same Compose project selector, but that does not make the lane safe to run against a live system without that assessment. Run these checks only in an authorized isolated environment with a separate Docker daemon, HOME, runtime and fixture data; isolated results alone do not establish acceptance of an operator's live environment.

   For acceptance, the operator must tie the evidence to the final release source: use a fresh checkout, record `git rev-parse HEAD` and a clean tracked working tree, and ensure the Docker build context has no extra files under the source paths copied by the Dockerfile. Build `target/debug/codex-memoryd` from that source and ensure any binary overrides for the gate, native helper, and sandbox identify that build. The heartbeat uses `target/debug/codex-memoryd` for its MCP canary. Keep the lane's database and service targets under operator control; never use the vulnerable v0.1.0 PID-file `down` or `restart`. Do not wrap `--include-dogfood` in the child-reaper launcher. A completed run's `release-gate-summary.json` must have `status == "pass"`, `operator_dogfood_included == true`, and no skipped or failed steps. The summary does not itself record the source SHA. Share only the content-free result and source identity; heartbeat and canary logs may contain private memory data. An operator can check the required result without running the lane with:

   ```bash
   jq -e '
     .status == "pass" and .operator_dogfood_included == true and
     ([.steps[] | select(.status != "pass")] | length == 0) and
     ([.steps[].label] as $labels |
       ["dogfood-compose-heartbeat", "dogfood-compose-down-before-native",
        "native-runtime-start", "native-runtime-status", "native-runtime-smoke",
        "native-runtime-restart-survival", "native-runtime-stop", "dogfood-write-sandbox-run"] |
       all(.[]; . as $required | $labels | index($required) != null))
   ' "$operator_summary"
   ```

   Expected: `true` and exit status zero. This checks the report structure; the operator must separately establish its source identity and authenticity.
7. Only after explicit release authorization, create the approved tag and manually dispatch the reviewed publisher on that same tag, supplying that tag and its exact reviewed commit as inputs. Publishing the stable GitHub release sets GitHub Releases `latest`; the publisher never moves GHCR `latest`. It can leave a draft or public release, untagged platform images, or a versioned image behind if a later step fails; inspect actual state before any retry and obtain authorization for any cleanup. GHCR tag assignment is not an atomic compare-and-swap against other writers, so other publishers must be stopped during the release and the resulting digest rechecked. Record the actual tag, exact release commit, immutable-release status, checksums, tap version, GHCR manifest digest, and provenance links. Only the separately authorized deployment coordinator may decide and execute a live rollout.

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

Expected: ancestry check and every test/gate exit zero, and the summary expression outputs `true`; record `git rev-parse HEAD` as the **future** tested release commit. The required live lane remains pending. On a disposable macOS runner, `cargo test --locked --test native_unsupported_safety` must pass and its disposable child remain alive; exercise foreground `serve` with a supervisor and temporary database, without relying on managed native `up`/`down`.

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

After an authorized publisher run, set `signer_workflow` to `joshyorko/codex-memoryd/.github/workflows/publish-release.yml` and `image_digest` to the verified published registry digest. Verify each signed subject, source commit, and tag using a current GitHub CLI:

```bash
for archive in "$dist"/codex-memoryd-v"$version"-*.tar.gz; do
  gh attestation verify "$archive" --repo joshyorko/codex-memoryd \
    --source-digest "$release_commit" --source-ref "refs/tags/$tag" \
    --signer-workflow "$signer_workflow"
done
gh attestation verify "oci://ghcr.io/joshyorko/codex-memoryd@$image_digest" \
  --repo joshyorko/codex-memoryd --source-digest "$release_commit" \
  --source-ref "refs/tags/$tag" --signer-workflow "$signer_workflow"
```

Expected: every verification exits zero for the actual release subjects and the approved signer. The OCI check requires registry authentication. See the [GitHub CLI attestation verification reference](https://cli.github.com/manual/gh_attestation_verify); a valid signature alone does not prove an independent rebuild matched.

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

Sol's review of release-preparation head `4dbfceb35494af71ca286dbd3fd8a1bf62d2dd02` found that a mutable OCI version tag could move between index validation and selection of the attestation subject. The proposed publisher now resolves one digest first, validates the platform list through that digest, and attests the same digest. Its later check rejects a version tag that no longer points to that subject. The regression tests execute the workflow shell with a disposable fake registry, including tag movement before and after subject selection; actual registry publication and signing remain untested. The corrected head needs a new independent review.

On 2026-10-29 GitHub reported seven Dependabot alerts on the default branch when the preparation branch was pushed, including one critical and one high. The connected GitHub tools did not expose those alert records, so their identifiers and open, dismissed, or stale status remain unknown. The candidate lockfile updates the root package version, `rustls-webpki`, and `tokio` as described below.

A separate read-only `cargo-audit` 0.22.2 scan against RustSec advisory database commit `ee1a49a2769a24a596afc1c20cb33748d2769586` reported eight vulnerable crate advisories and one unsoundness warning. The critical [RUSTSEC-2026-0369](https://rustsec.org/advisories/RUSTSEC-2026-0369.html) and high [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367.html) affect `zip` 2.4.2, a direct production dependency, when its extraction APIs are used. The candidate's Linux and macOS dependency graphs include the crate, but a source search found no `ZipArchive::extract` or `ZipStreamReader::extract` calls: the bundle and ChatGPT import code reads archive entries instead. This is a bounded call-site assessment, not proof about GitHub's inaccessible alerts or a complete audit of ZIP handling.

The same scan identified [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), affecting `rustls-webpki` 0.103.15 through production `reqwest` on Linux and macOS. The HTTPS provider path uses the system trust store and can send memory-derived input and a bearer credential; a successful interception could expose or alter that traffic. Exposure depends on whether the configured trust anchors accept paths through the affected intermediates. No live configuration was inspected and exploitation was not reproduced. The candidate updates that dependency to the compatible patched version 0.103.16; rechecking against the same RustSec database no longer reports this advisory.

The candidate also raises the direct Tokio minimum and lockfile from 1.53.1 to 1.53.3 for [RUSTSEC-2026-0386](https://rustsec.org/advisories/RUSTSEC-2026-0386.html). The project's Rust 1.88 toolchain exceeds Tokio's 1.71 minimum. A static search of the source and the normal dependency graphs for all four release targets found no `AsyncFd` use outside Tokio itself. Tokio 1.53.3 retains the legacy safe constructors as deprecated wrappers, however, so updating the version does not make a caller that misuses them sound. The absence of a caller is part of this candidate's assessment; recheck it if dependencies or features change. The same RustSec database no longer reports the Tokio warning. Seven vulnerable-crate advisories remain, so the audit still exits nonzero. It does not establish the status of GitHub's inaccessible Dependabot alerts, and the scan did not assess container OS packages, CI actions, or complete reachability of every advisory. Revalidate the final source and platform artifacts before publication.

Issues [#270](https://github.com/joshyorko/codex-memoryd/issues/270) and [#271](https://github.com/joshyorko/codex-memoryd/issues/271) remain open and separate from PR #275.
