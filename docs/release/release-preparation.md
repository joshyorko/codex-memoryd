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
| Proposed next stable version | The draft source version is `0.1.1`; `v0.1.1` remains a proposal and has not been tagged or approved for publication. `v0.1.0` is the sole current stable release and existing tag. |
| Release tag and release source commit | **Pending**: no new tag or release exists. A future version/workflow commit must include the merge above; record its own exact SHA after validation. |
| Native binary archive checksums | **Pending** actual published artifacts and verification. |
| Homebrew version and artifact checksums | **Pending** decision and publication of a verified formula. The currently published master-tracking formula is `20261014222239.79a279d5f730`, from pre-fix commit `79a279d5f730162ad34be9b6410e666ba878a05c`; it does not include #275. |
| GHCR image and manifest digest | **Pending** actual published image and verification. |
| Release provenance and reproducibility | **Pending** a future authorized publisher run on the exact release tag, independent rebuild comparison, signed attestations, and verification. CI candidate evidence is not release provenance. |
| Published / deployed | No new stable release, tag, Homebrew update, GHCR image, or deployment was created by this worker. |

The existing v0.1.0 native binary and the currently published Homebrew build predate the fix. Never use their native `down` or `restart` based on a PID file. Do not assume a new manager can manage the old process or its legacy PID file.

## Decisions for the operator

Josh has accepted the fail-closed macOS behavior for this release: managed native `up`, `down`, and `restart` do not operate; foreground `serve` under a trusted supervisor and container mode remain supported. Native status reports the process as unverified. The proposed release is `v0.1.1`; its final version and publication are not yet approved.

Once the release source and reviews are final, the bundled operator decision should ask: “If the required automatic connector review has not completed for PR #277's final head, approve one manual `@codex review` request? After that review and CI pass, approve merging the release-preparation PR; separately, approve the scoped Homebrew publisher guard and its source-SHA variable, and provide the read-only immutability-audit credential described below? Once those gates and the security assessment pass, approve tagging `v0.1.1` at [exact reviewed release commit], then publishing its verified GitHub release, Homebrew update, and versioned GHCR image? Separately, should GHCR `latest` move to the verified image digest?” The source SHA is pending until the release changes and their reviews are final. Approval to publish does not authorize deployment. This preparation does not create or approve any of those artifacts.

## What must be prepared and reviewed before release

1. Confirm the release version. The fix supports race-resistant managed native lifecycle on Linux when pidfds, readable `/proc`, and required permissions are available. Josh accepted fail-closed managed native `up`, `down`, and `restart` on macOS and other non-Linux platforms; foreground `serve` under a suitable supervisor and container mode remain alternatives. This is a material change from v0.1.0. `CODEX_MEMORYD_BIN` must resolve to the manager's same executable inode; symlinks and hard links are supported, wrappers and different binary builds are not.
2. Review the proposed version-specific source change, including `Cargo.toml`, the root package entry in `Cargo.lock`, version checks and documentation. `.github/workflows/release-v0.1.0.yml` is explicitly locked to the old tag; a future tag does **not** trigger it. The new `.github/workflows/publish-release.yml` is a version-neutral, manual-only publisher. It requires the selected tag, the explicitly supplied full commit SHA, the package version, and master ancestry to match; a tag or branch push does not invoke it. It calls the read-only `.github/workflows/release-preflight.yml` on that exact source, requires the full synthetic gate, verifies same-run candidate pairs, signs and verifies native subjects, stages and checks a draft release, then rebuilds and checks both OCI platform digests before assigning the versioned image and signing and verifying its index. The draft is looked up through the authenticated release list because GitHub's release-by-tag endpoint returns published releases. The workflow does not assign GHCR `latest`. This workflow is preparation code and has not been run through actual publication or signed an artifact. Do not create a tag or dispatch the publisher during preparation.
   The publisher fails closed unless `RELEASE_IMMUTABILITY_AUDIT_TOKEN` is configured as a repository secret with **Administration: read** for this repository only. GitHub's [immutability-settings endpoint](https://docs.github.com/rest/repos/repos#check-if-immutable-releases-are-enabled-for-a-repository) requires that permission, which the ordinary workflow token cannot request. Creating or granting that credential is a separate operator action; never paste the token into chat. The publisher checks that immutable releases are enabled before external writes and again before publishing the release. A missing or insufficient token stops the run. A current GitHub CLI with attestation and immutable-release verification commands is also required and checked before writes.
3. Coordinate Homebrew as its own release channel. The actual tap is https://github.com/joshyorko/homebrew-tools. `Formula/codex-memoryd.rb` currently uses timestamp-plus-commit versions and tap-hosted assets; it does not automatically use the stable GitHub release archive or its semver. The tap's `codex-memoryd-update.yml` rebuilds latest master on four platforms, then publishes a tap release and commits the formula, if all jobs pass. Its macOS lifecycle smoke, and the formula's own macOS `brew test`, currently call managed `up` and `down` and will fail with this fix. Decide whether to make the primary formula consume the verified stable release, or retain a clearly separate master-tracking channel. In either case update and review the publisher and tests, then verify every formula URL and checksum against the intended artifact provenance. The currently observed tap-hosted release has `immutable: false`; do not describe that asset set as immutable without fixing or independently establishing that property.
4. The tap has a daily scheduled `codex-memoryd-daily` job at **10:37 UTC**, which attempts to rebuild latest master and can publish without a new MemoryD tag. The current macOS smoke should stop this particular commit before publication, but that failure must not be treated as an authorization gate or a guarantee about future workflow changes. A narrowly scoped proposed safeguard is to gate the tap's `publish` job on a nonempty operator-approved source SHA that equals `needs.resolve.outputs.commit`; retain the shared schedule and other packages. Implementing that tap change or setting its approval variable requires separate authorization. No tap workflow or settings were changed here.
   A proposed job-level condition in `homebrew-tools/.github/workflows/codex-memoryd-update.yml` is:

   ```yaml
   if: >-
     vars.CODEX_MEMORYD_APPROVED_SOURCE_SHA != '' &&
     needs.resolve.outputs.commit == vars.CODEX_MEMORYD_APPROVED_SOURCE_SHA
   ```

   This gate covers that workflow's publish job for scheduled and manual calls; it does not control other tap workflows or direct publication. After the tap change is reviewed, an operator would set the variable only to an explicitly approved source SHA. The condition and variable have not been installed.
5. Establish artifact evidence before publication. The read-only preflight records unsigned, seven-day CI candidate artifacts and compares two separate runner builds for each native target and each Linux OCI platform. It then runs the publisher's candidate guards on those same-run artifacts without producing a release bundle. It compares the OCI exporter with the publisher's non-pushing image exporter, sets a source timestamp, and normalizes exported layers; it does not push an image. Its OCI comparison excludes embedded attestations and SBOMs while retaining unsigned Buildx metadata separately. Neither the candidate digest nor metadata is a published artifact identity or signed attestation. The prepared publisher creates separate GitHub provenance attestations for the actual native files and OCI index, and verifies them against the tag, commit, and publisher workflow. Those operations are untested against the real attestation service because publication is unauthorized. The Dockerfile still uses a floating frontend and mutable `apt-get update`; runner images, Buildx, and BuildKit may change. The publisher requires matching build tools and fails if the final local or pushed platform digest differs from the two candidates. This detects drift for that run; it does not make upstream repositories or the registry immutable. If any comparison or attestation check fails, reproducibility or provenance remains unproven.
   Independent preflight builds found different OCI manifest digests on both Linux architectures: https://github.com/joshyorko/codex-memoryd/actions/runs/40922940280. Layer diagnostics isolated the differences to timestamped apt/dpkg logs and the linker auxiliary cache produced during package installation. After those build-only files were removed, independent OCI comparisons and disposable container health checks passed for both architectures at `625a35df29c51a82d27c276642b880df96e8cd57`: https://github.com/joshyorko/codex-memoryd/actions/runs/40924143300. Recheck the final release source; the preflight retains file-hash diagnostics, not temporary OCI image archives. Rerun the whole workflow rather than only failed matrix jobs so both build slots share one run attempt.
   Prior macOS ARM candidate comparisons found identical binaries but different compressed archives: https://github.com/joshyorko/codex-memoryd/actions/runs/40922940280. All four native comparisons subsequently passed at `625a35df29c51a82d27c276642b880df96e8cd57`: https://github.com/joshyorko/codex-memoryd/actions/runs/40924143300. A later diagnostic run identified the cause: runners used Python 3.14.7 and 3.15.2 with the same zlib version; their binaries and uncompressed tar bytes matched but gzip output differed. The first attempted pin, 3.12.11, was unavailable on the macOS runners. The preflight now pins Python 3.14.10, which the Actions version manifest lists for all four targets, and records Python/zlib versions and uncompressed tar hashes. A fresh comparison must pass on the exact final release source.
6. Run the focused checks and full synthetic gate below on the final release source, obtain the required independent/repository reviews, and make sure all final-head CI passes. The existing repository documentation additionally marks live dogfood checks as tag-required; those checks were **not run** here. Its older operator instructions include service and Compose lifecycle actions, so they must be reviewed for PID safety and replaced or separately authorized by the deployment coordinator where appropriate. Do not run those live checks from this cloud worker.
7. Only after explicit release authorization, create the approved tag and manually dispatch the reviewed publisher on that same tag, supplying that tag and its exact reviewed commit as inputs. Publishing the stable GitHub release sets GitHub Releases `latest`; the publisher never moves GHCR `latest`. It can leave untagged platform images, a versioned image, or a draft release behind if a later step fails; inspect actual state before any retry and obtain authorization for any cleanup. GHCR tag assignment is not an atomic compare-and-swap against other writers, so other publishers must be stopped during the release and the resulting digest rechecked. Record the actual tag, exact release commit, immutable-release status, checksums, tap version, GHCR manifest digest, and provenance links. Only the separately authorized deployment coordinator may decide and execute a live rollout.

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

On 2026-10-29 GitHub reported seven Dependabot alerts on the default branch when the preparation branch was pushed, including one critical and one high. The connected GitHub tools did not expose those alert records, so their identifiers and open, dismissed, or stale status remain unknown. The proposed lockfile at `724941e06d9a5ab28f06ab32f47f340a2ed212b6` differs from master only in the root package version.

A separate read-only `cargo-audit` 0.22.2 scan against RustSec advisory database commit `ee1a49a2769a24a596afc1c20cb33748d2769586` reported eight vulnerable crate advisories and one unsoundness warning. The critical [RUSTSEC-2026-0369](https://rustsec.org/advisories/RUSTSEC-2026-0369.html) and high [RUSTSEC-2026-0367](https://rustsec.org/advisories/RUSTSEC-2026-0367.html) affect `zip` 2.4.2, a direct production dependency, when its extraction APIs are used. The candidate's Linux and macOS dependency graphs include the crate, but a source search found no `ZipArchive::extract` or `ZipStreamReader::extract` calls: the bundle and ChatGPT import code reads archive entries instead. This is a bounded call-site assessment, not proof about GitHub's inaccessible alerts or a complete audit of ZIP handling.

The same scan identified [RUSTSEC-2026-0382](https://rustsec.org/advisories/RUSTSEC-2026-0382.html), affecting `rustls-webpki` 0.103.15 through production `reqwest` on Linux and macOS. The HTTPS provider path uses the system trust store; exposure depends on whether the configured trust anchors accept paths through the affected intermediates. That configuration was not inspected on any live system. Assess this TLS risk and the actual Dependabot records before approving publication. The RustSec scan did not assess container OS packages, CI actions, or the complete reachability of every advisory.

Issues [#270](https://github.com/joshyorko/codex-memoryd/issues/270) and [#271](https://github.com/joshyorko/codex-memoryd/issues/271) remain open and separate from PR #275.
