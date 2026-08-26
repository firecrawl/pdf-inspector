# Publishing

Every pdf-inspector distribution uses one shared semantic version:

- Rust crate: `pdf-inspector`
- Python package: `pdf-inspector`
- Node package: `@firecrawl/pdf-inspector` and its platform packages
- Browser package: `@firecrawl/pdf-inspector-wasm`
- .NET package: `Firecrawl.PdfInspector`
- Internal NAPI, WASM, and .NET native Rust crates

`Cargo.toml` is the canonical version source. Update every manifest and lockfile
with:

```bash
python3 scripts/version.py <version>
```

Verify that nothing has diverged with:

```bash
python3 scripts/version.py --check
```

CI and every publishing workflow run this check before building or publishing.

## Release steps

1. Choose the next shared semantic version and run `scripts/version.py`.
2. Review the manifest and lockfile changes in the version-bump pull request.
3. Merge the pull request to `main`.
4. The crates.io, PyPI, Node, WASM, and NuGet workflows independently build and
   publish that version from the same commit.
5. After all registries succeed, create one `v<version>` GitHub release that
   links to each package and describes changes since the previous shared tag.

The independent workflows are intentionally idempotent. A manual dispatch from
`main` can repair a partial release, and already-published artifacts are skipped.

## Trusted publishers

The repositories use GitHub Actions OIDC instead of long-lived registry tokens.
Configure each registry's trusted publisher for `firecrawl/pdf-inspector` and
its corresponding workflow:

- crates.io: `publish-crate.yml`, environment `crates-io`
- PyPI: `publish-pypi.yml`, environment `pypi`
- npm Node package: `publish.yml`
- npm WASM package: `publish-wasm.yml`
- NuGet: `publish-nuget.yml`, environment `nuget`

The WASM package must exist before npm trusted publishing can be configured. If
it ever needs to be bootstrapped again, build and inspect it before publishing:

```bash
wasm-pack build wasm --target web --scope firecrawl --out-dir pkg --release
npm pack --dry-run ./wasm/pkg
npm publish ./wasm/pkg --access public
```

The NuGet workflow uses NuGet.org trusted publishing through `NuGet/login`.
Configure a policy for `firecrawl/pdf-inspector`,
`.github/workflows/publish-nuget.yml`, and the `nuget` environment, then store
the NuGet.org user/profile name in the `NUGET_USER` repository secret. The
workflow assembles one package containing all supported RID-specific native
libraries.

### NuGet package-only dry run

`publish-nuget.yml` can be run manually from the Actions page. Choose **Run
workflow**, select the branch or commit to test, and start the run. A manual
run:

1. Builds and tests all four native RID libraries.
2. Assembles and validates the complete `.nupkg`.
3. Installs the package into the clean smoke project.
4. Uploads the `.nupkg` and `.snupkg` in the `nuget-package` workflow artifact
   for seven days.
5. Skips the NuGet login and publish job unconditionally.