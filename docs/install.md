# Installing Pingers on Android

Pingers is not in an app store yet. Each version is published as a
[GitHub Release](https://github.com/OceanEilonwy/pingers/releases), and
[Obtainium](https://github.com/ImranR98/Obtainium) installs it from there and keeps it up to
date. Obtainium is a free app that installs apps straight from where their developers publish
them; it never adds anything to them.

## 1. Choose your version

Every release has two APKs. They have the same features but are separate apps to Android
(`org.pingers.app` and `org.pingers.app.foss`), so pick one and stay with it.

| You have | Install | APK filter in Obtainium |
|---|---|---|
| Google Play services (most phones) | `gms`: notifications through Google's push service (FCM) or a UnifiedPush app | `pingers-.*-gms\.apk$` |
| No Google Play services (GrapheneOS without sandboxed Play, LineageOS without GApps, /e/OS, and so on) | `foss`: no Google code at all; notifications through a UnifiedPush app such as ntfy | `pingers-.*-foss\.apk$` |

Not sure? If the Play Store app works on your phone, choose `gms`.

## 2. Add Pingers to Obtainium

1. Install Obtainium from its [releases page](https://github.com/ImranR98/Obtainium/releases)
   (or from F-Droid or IzzyOnDroid).
2. In Obtainium, tap **Add App**.
3. **App source URL**: `https://github.com/OceanEilonwy/pingers`
4. Open the additional options and set **Filter APKs by regular expression** to the filter for
   your version from the table above, for example `pingers-.*-foss\.apk$`.
5. Leave **Include prereleases** off unless you want to test release candidates (versions like
   `1.0.0-rc.1`). Pre-releases are marked as such on GitHub and may have rough edges.
6. Tap **Add**, then **Install**. Android asks you to allow Obtainium to install apps the first
   time.

## 3. Check the signing certificate

Every Pingers APK is signed by the same key. Its certificate's SHA-256 fingerprint is:

```
AA:44:E4:54:B0:F8:FA:A8:9B:8F:69:EF:72:90:6D:AA:2A:B8:C3:F2:D6:BE:87:B1:23:C2:D3:30:C9:BD:18:16
```

It is also in every release's notes. Check it once, after the first install:

- **On the phone, with [AppVerifier](https://github.com/soupslurpr/AppVerifier):** open it, pick
  Pingers (package `org.pingers.app`, or `org.pingers.app.foss` for foss), and compare the
  fingerprint it shows with the one above. You can also copy the fingerprint above and share it
  to AppVerifier, which then says whether it matches.
- **On a computer**, with the Android SDK's build tools, before installing:
  `apksigner verify --print-certs pingers-<version>-<flavour>.apk` prints
  `Signer #1 certificate SHA-256 digest:` followed by the same value, in lower case and without
  colons.

After that Android itself checks every update: it refuses to update an app with an APK signed
by a different key, so a later APK that is not from Pingers cannot replace yours.

Each release also lists the SHA-256 of every file, in its notes and in an attached
`SHA256SUMS` (`sha256sum -c SHA256SUMS` checks downloaded files).

## 4. Updates

Obtainium checks for new releases in the background (Settings > background update checking)
and notifies you when one is out. Tap the notification, or open Obtainium and tap **Update**,
and Android installs it over your current version; your Parties, Spots and settings stay. On
Android 12 and later Obtainium can install updates without asking each time.

Versions only move forward: every release has a larger version number than the one before, so
Android always accepts it as an update.

## Your first phone on your own appliance

If you run the Pingers server on your own monokulo-nodo appliance, install Pingers as above,
then open the appliance's Setup page (in nodo-tui: Apps, pingers, Setup) and scan its
first-phone QR code with your phone's camera. Pingers opens on "Set up Pingers on this phone" and
makes your account with the server's registration code; everyone else joins through your invite
links. docs/deploy-nodo.md has the whole setup.

## Switching between gms and foss

The two are separate apps, and neither updates the other. To switch, add the other version to
Obtainium as a new app (same URL, the other APK filter), install it, then uninstall the old one
in Obtainium. Nothing carries over between them: you rejoin your Parties with new invites in
the new app.
