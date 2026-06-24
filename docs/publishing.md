# Publishing

The Rust crate is published to [crates.io](https://crates.io/crates/pdf-inspector) with trusted publishing from GitHub Actions. The first release was published manually; future releases publish from `.github/workflows/publish-crate.yml` when a `Cargo.toml` version change lands on `main`.

## crates.io Trusted Publisher

Configure the trusted publisher for the `pdf-inspector` crate with:

- Repository: `firecrawl/pdf-inspector`
- Workflow: `publish-crate.yml`
- Environment: `crates-io`

The workflow uses `rust-lang/crates-io-auth-action@v1` to exchange GitHub's OIDC token for a short-lived crates.io token, then passes it to `cargo publish`.

## Release Steps

1. Update `version` in `Cargo.toml`.
2. Merge the version bump to `main`.
3. The publish workflow compares the new `Cargo.toml` version with `HEAD~1`, runs `cargo publish --dry-run`, then publishes if that version is not already on crates.io.

If `Cargo.toml` changes without a package version bump, the workflow exits without publishing.

## Legacy npm Package

The unscoped `firecrawl-pdf-inspector` package is deprecated in favor of `@firecrawl/pdf-inspector`.

The npm publish workflow publishes the compatibility wrapper in `napi/legacy` whenever `napi/legacy/package.json` has a version bump on `main`. The wrapper keeps older installs working while its README points users to the scoped package.

Keep `napi/legacy/package.json` on the same version as `napi/package.json`, and pin its `@firecrawl/pdf-inspector` dependency to that same version. The publish workflow fails fast if the package versions drift.

Configure npm trusted publishing for both packages against `.github/workflows/publish.yml`:

- `@firecrawl/pdf-inspector`
- `firecrawl-pdf-inspector`

To mark the legacy package as deprecated from CI, configure a GitHub Actions `NPM_TOKEN` secret with permission to manage `firecrawl-pdf-inspector`. Without that secret, the workflow still publishes the wrapper but skips `npm deprecate`.

You can also deprecate the legacy package manually from an npm account that owns it:

```bash
npm deprecate firecrawl-pdf-inspector "Deprecated: this package has moved to @firecrawl/pdf-inspector. Please install @firecrawl/pdf-inspector instead."
```

If the account has two-factor auth enabled, append `--otp=<code>`.
