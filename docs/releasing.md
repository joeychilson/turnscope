# Releasing

A release is a tag `vX.Y.Z` on main. The release workflow
(`.github/workflows/release.yml`) builds, signs, notarizes and publishes it,
and every installed copy is offered it through Sparkle. A version published
is never changed or taken back: a bad release is fixed by the next one.

## Once: set up signing and updates

Everything below is done once, by the owner, on a Mac that holds the Developer
ID. `gh` is the GitHub CLI, signed in to the repository.

### The Sparkle key

Updates are signed with an EdDSA key. Every copy carries its public half and
refuses an update not signed with the private half, so **losing the private
key means no installed copy can be updated again.** Back it up, as you would
the Developer ID.

```sh
# Sparkle's tools, of the version the app uses.
sparkle=$(jq -r '.pins[] | select(.identity == "sparkle") | .state.version' \
  macos/Turnscope.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved)
mkdir -p "target/sparkle-$sparkle"
curl -sSfL "https://github.com/sparkle-project/Sparkle/releases/download/$sparkle/Sparkle-$sparkle.tar.xz" |
  tar -xJ -C "target/sparkle-$sparkle"

# Make the key, kept in your login keychain, and print its public half.
"target/sparkle-$sparkle/bin/generate_keys"

# The public half, a repository variable.
gh variable set SPARKLE_PUBLIC_KEY --body "$("target/sparkle-$sparkle/bin/generate_keys" -p)"

# The private half, a repository secret; then remove the exported file.
"target/sparkle-$sparkle/bin/generate_keys" -x sparkle-private-key
gh secret set SPARKLE_PRIVATE_KEY < sparkle-private-key
rm -P sparkle-private-key
```

`generate_keys` reuses a key already in the keychain rather than make a new
one. On another Mac, `generate_keys -f <file>` imports an exported key.

### The Developer ID

The app and its disk image are signed with a **Developer ID Application**
certificate, and notarized with an App Store Connect API key.

1. In Keychain Access, export the Developer ID Application certificate with
   its private key as a `.p12`, with a password.
2. In App Store Connect, under Users and Access › Integrations › App Store
   Connect API, make a team key with the Developer role, and download its
   `.p8` file. Note its key ID and the issuer ID above the list.
3. Store them:

```sh
base64 -i DeveloperID.p12 | gh secret set DEVELOPER_ID_CERTIFICATE
gh secret set DEVELOPER_ID_PASSWORD          # the .p12's password
gh secret set NOTARY_KEY < AuthKey_XXXXXXXXXX.p8
gh secret set NOTARY_KEY_ID --body XXXXXXXXXX
gh secret set NOTARY_ISSUER --body xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx
```

Then delete the `.p12` and `.p8` files, keeping your backups of them.

### The repository

- **Actions**: allowed, with the default workflow permissions left read-only;
  each workflow asks for what it needs, and only the publishing job can write.
- **Main**: a ruleset that blocks deleting it and force pushes to it, and
  requires a linear history; the repository allows only squash merges, and
  deletes a branch once it is merged. Changes reach main as pull requests
  once Verify's checks, `rust`, `app` and `release-build`, pass. Main doesn't
  require pull requests, as a release's commit is pushed to it directly
  (step 3).
- **Tag protection**: a ruleset for tags matching `v*` that only you can
  create, update or delete, since a tag is a release.
- **Private vulnerability reporting**: on, under Security, as `SECURITY.md`
  asks people to use it.

## Try a release build here

`scripts/release.sh` runs as the workflow does. With nothing set it signs ad
hoc and notarizes nothing; with the release's environment it makes exactly
what would ship, in `target/dist/`:

```sh
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_KEY=/path/to/AuthKey_XXXXXXXXXX.p8 NOTARY_KEY_ID=XXXXXXXXXX NOTARY_ISSUER=xxxxxxxx-... \
SPARKLE_PUBLIC_KEY=... SPARKLE_PRIVATE_KEY="$(cat sparkle-private-key)" \
scripts/release.sh
scripts/smoke.sh
```

It stops if the Sparkle private key isn't the public key's pair, since every
copy would refuse an update signed with it.

## Cut a release

1. **The notes.** In `CHANGELOG.md`, rename `## [Unreleased]` to
   `## [X.Y.Z]`, add an empty `## [Unreleased]` above it and a link for the
   version at the bottom. These are the release notes, and what Sparkle shows
   before an update installs, so write them for the people using the app.
2. **The version.** Set `version` under `[workspace.package]` in
   `Cargo.toml` to `X.Y.Z`, and run `cargo check` to update `Cargo.lock`.
   The release build takes the app's version from there; the
   `MARKETING_VERSION` in the Xcode project is only for development builds,
   and can be set to match.
3. **Commit.** Commit as `chore(release): release X.Y.Z` directly on main
   and push it: a release changes no code, so it needs no pull request,
   which code changes go through and land squashed, one commit each. Main
   takes only commits added on top. Wait for Verify to pass on main.
4. **Tag.** On that commit on main:

   ```sh
   git tag -a vX.Y.Z -m "Turnscope X.Y.Z"
   git push origin vX.Y.Z
   ```

5. **Watch the run.** `gh run watch`, or the Actions tab. The workflow:
   - checks, in seconds, that the tag matches `Cargo.toml`, is on main and
     isn't released already, that `CHANGELOG.md` has notes for it, and that
     every secret and the variable is set;
   - runs Verify;
   - builds the app for Apple silicon and Intel with `scripts/release.sh`:
     signed inside out with the Developer ID, notarized and stapled, then the
     disk image signed, notarized and stapled, then the appcast, signed with
     the Sparkle key and checked against the public key;
   - runs `scripts/smoke.sh` on the result and checks its signature,
     Gatekeeper, the staples, its version and its public key;
   - makes a draft release on the tag with the disk image, the appcast and the
     changelog's notes, and only then publishes it as the latest.

   A run that fails leaves at most a draft. Fix the cause and re-run the
   failed jobs; the draft is replaced. If the fix needs a new commit, delete
   the tag (`git push --delete origin vX.Y.Z`, and locally), and tag again.
   Once published, never delete or move a tag: release the next version.

## Check the release

1. **The download.** From the release page, open the disk image and drag
   Turnscope to Applications on a Mac that hasn't had it; it opens without a
   Gatekeeper warning. `spctl --assess --type open --context
   context:primary-signature -vv Turnscope-X.Y.Z.dmg` says `accepted` and
   `Notarized Developer ID`.
2. **The appcast.** `curl -sL
   https://github.com/joeychilson/turnscope/releases/latest/download/appcast.xml`
   shows the new version, with an `edSignature`.
3. **The update.** In a copy of the previous release, Settings › General ›
   Check for Updates… offers the new version with its notes, installs it and
   relaunches it; Settings then shows the new version. For the first release,
   check this with the second.
