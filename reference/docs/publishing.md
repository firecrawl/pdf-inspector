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

## Browser WebAssembly package

The browser package is published as `@firecrawl/pdf-inspector-wasm`. Its version lives in `wasm/Cargo.toml`, and `.github/workflows/publish-wasm.yml` builds the `web` target with `wasm-pack` before publishing the generated package.

The npm package must exist before a trusted publisher can be configured. For the first release only:

1. Build with `wasm-pack build wasm --target web --scope firecrawl --out-dir pkg --release`.
2. Inspect with `npm pack --dry-run ./wasm/pkg`.
3. Publish with `npm publish ./wasm/pkg --access public` from an authorized maintainer session.
4. In the package settings on npm, configure the GitHub Actions trusted publisher:
   - Organization: `firecrawl`
   - Repository: `pdf-inspector`
   - Workflow: `publish-wasm.yml`
   - Allowed action: `npm publish`

After that one-time bootstrap, bumping the version in `wasm/Cargo.toml` and merging it to `main` publishes through OIDC. Until the package exists, the workflow exits cleanly without attempting an unauthenticated first publish. See npm's [trusted publishing documentation](https://docs.npmjs.com/trusted-publishers/) for the registry-side setup.
