# Releasing

1. Bump `version` in `Cargo.toml`, and `version_code` under
   `[package.metadata.fdroid]` to match:
   `(1 << 24) | major << 16 | minor << 8 | patch`. For example, 0.0.6 is
   16777222 and 0.1.0 is 16777472. This is the versionCode cargo-apk writes
   into the APK, so every part of the version must stay below 256.
2. Write `fastlane/metadata/android/en-US/changelogs/<version_code>.txt`
   (plain text, 500 characters at most). F-Droid and IzzyOnDroid show it as
   the release notes, and the release workflow also uses it as the GitHub
   release body, which is what Zapstore shows.
3. Commit, tag `vX.Y.Z` and push the tag. `.github/workflows/release.yml`
   checks steps 1 and 2 before building anything, then publishes the GitHub
   release with the signed APK.

## Android app stores

All three stores read the store listing from `fastlane/metadata/android/en-US`
(title, descriptions, icon, screenshots, changelogs). Edit it there.
The screenshots are the phone renders from
`cargo run --example screenshots`, converted to PNG.

### Zapstore

Zapstore is a Nostr app store. It ships the APK from the GitHub release
(our signature, like IzzyOnDroid), and `zsp` reads the listing from the
fastlane folder and the release notes from the GitHub release body.
`zapstore.yaml` holds the rest: the repo, the tags and the publisher's
npub. The relay whitelists that npub because the file is committed in this
repo, so keep `pubkey` in sync with whoever publishes.

First publish, by hand, after the GitHub release exists:

1. Install the publisher: `go install github.com/zapstore/zsp@latest`.
2. `zsp publish --check zapstore.yaml` confirms it finds the arm64 APK.
3. `SIGN_WITH=browser zsp publish zapstore.yaml` (or a `bunker://` URL).
   The first run asks to link the APK signing certificate to the npub
   (NIP-C1); point it at `release.jks`. It shows a preview before it
   publishes anything.

After that, the `zapstore` job in `.github/workflows/release.yml` publishes
each tagged release. It needs a `ZAPSTORE_SIGN_WITH` secret holding a NIP-46
bunker URL (not an nsec), and skips itself while the secret is unset.

### IzzyOnDroid

IzzyOnDroid ships the APK attached to the GitHub release, signed with our
key, so there is nothing to build on their side. To get listed, open a
request at <https://gitlab.com/IzzyOnDroid/repo/-/issues> with the repo URL.
After that, new releases are picked up automatically.

### F-Droid

F-Droid builds the APK from the tagged source and signs it with its own
key. Because the keys differ, users can't switch between the F-Droid build
and the GitHub/IzzyOnDroid build without uninstalling first.

`fdroid/com.flipflop.app.yml` is the build recipe to submit. To submit it,
fork <https://gitlab.com/fdroid/fdroiddata> and copy the recipe to
`metadata/com.flipflop.app.yml`. Test it with `fdroid build -v -l
com.flipflop.app`, then open a merge request. The recipe was written by
hand and hasn't run on F-Droid's build server, so expect to adjust it
there. fdroiddata's CI rejects recipes that `fdroid rewritemeta` would
reformat, and that strips comments, so the reasoning lives here. A release
build won't finish without a keystore, so the recipe creates a throwaway one
and ships `flipflop-unaligned.apk`. That file is the complete APK before
zipalign and signing; F-Droid aligns and signs it itself. After the merge, the update checker finds new `vX.Y.Z` tags and reads
the version and `version_code` from `Cargo.toml`.

Points the F-Droid reviewers may raise:

- **Relays**: by default, peers that can't reach each other directly
  connect through n0's public iroh relays. The relay software is free
  software, and relaying can be switched to custom servers or turned off.
- **`rustls-platform-verifier-android`** (pulled in by `iroh-relay`) ships a
  prebuilt `.aar` in its crate. It sits in the cargo registry, not in this
  repo, and isn't packaged into the APK.
