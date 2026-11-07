# Releasing tab

`.github/workflows/release.yml` turns a version number into installers on a GitHub release:

| Platform | What you get |
|---|---|
| macOS, Apple silicon | `tab_X.Y.Z_aarch64.dmg` |
| Windows x64 | `tab_X.Y.Z_x64-setup.exe` (NSIS) and `tab_X.Y.Z_x64_en-US.msi` |
| iOS | not yet (see the end) |

Signing is optional. With no secrets set, the workflow still builds and publishes everything, unsigned. Each secret you add turns its part of signing on, with no change to the workflow. The run's summary page says what was signed.

## Cutting a release

**The usual way:** Actions → **Release** → **Run workflow** on `main`, and enter the version, like `v1.2.3`. The run:

1. **Checks the version.** It must be strict `vMAJOR.MINOR.PATCH` (`v1.2.3`, not `1.2.3`, `v1.2`, `v01.2.3` or `v1.2.3-beta`). It must not match any existing tag or release, drafts included. A version lower than an existing one is allowed (a hotfix on an older line) with a warning, because installed copies only update to newer versions.
2. **Runs the checks.** It runs the PR checks from `ci.yml` (UI typecheck and build, `tab-core` and shared crate tests, the Wasm build, proto lint and codegen). It also runs `cargo fmt --check`, clippy (correctness and suspicious lints fail the release, style lints only warn), and the full storage suite with `--perf`, which is too slow for every PR.
3. **Creates a draft release** with generated notes. A draft has no tag yet, so a failed build leaves no tag behind.
4. **Builds** macOS (Apple silicon only) and Windows in parallel. It stamps the version into the app (there's no version bump commit; `tauri.conf.json` stays at `0.1.0`) and uploads the installers to the draft.
5. **Publishes** the release and marks it latest. This happens only if every build passed.

If a build fails, fix it and use **Re-run failed jobs**, which reuses the same draft. To start over instead, delete the draft first, or the version check will refuse the version as taken.

Tick **dry run** to do everything except create the release. The installers are attached to the run as artifacts. Run it from `main` to test signing; from any other branch it builds unsigned.

**From the GitHub UI:** publishing a release with a new `vX.Y.Z` tag also starts the workflow, and the installers are attached to that release when the builds finish. The release is public while it builds, and a failed check leaves it without installers, so the Run workflow way is safer.

## Secrets and variables

Put them in a GitHub **environment** named `macos - dmg`, so only `main` and release tags can use them:

1. Go to Settings → **Environments** → **New environment**, and name it `macos - dmg`.
2. Under **Deployment branches and tags**, choose **Selected branches and tags**. Add the branch `main` and the tag pattern `v*`.
3. Add the entries below under **Environment secrets** and **Environment variables**.

A workflow run from any other branch, including a dry run, then can't read them and builds unsigned. Plain repository secrets (Settings → Secrets and variables → Actions) work too, but any branch could read those.

Everything is optional. Each group only takes effect once all of its required entries are set.

| Name | Kind | What it is |
|---|---|---|
| `APPLE_CERTIFICATE` | secret | Developer ID Application `.p12`, base64 |
| `APPLE_CERTIFICATE_PASSWORD` | secret | The password you gave the `.p12` |
| `APPLE_SIGNING_IDENTITY` | secret | Recommended. The identity's name, like `Developer ID Application: Vincent Costa (AB12CD34EF)` |
| `APPLE_API_KEY_P8` | secret | Notarization: the contents of `AuthKey_XXXX.p8` |
| `APPLE_API_KEY_ID` | secret | Notarization: the key's ID |
| `APPLE_API_ISSUER` | secret | Notarization: the Issuer ID |
| `APPLE_ID`, `APPLE_PASSWORD`, `APPLE_TEAM_ID` | secrets | Notarization, the alternative to the API key: Apple ID, app-specific password, Team ID |
| `TAURI_SIGNING_PRIVATE_KEY` | secret | Updater signing key (the file's contents) |
| `TAURI_SIGNING_PRIVATE_KEY_PASSWORD` | secret | Its password |
| `TAURI_UPDATER_PUBKEY` | variable | The updater key's public half. Built into the app so it can verify updates |
| `TAB_SUPABASE_URL` | variable | The tab Supabase project, `https://<ref>.supabase.co`. Built in so the app can sign in to tab cloud |
| `TAB_SUPABASE_ANON_KEY` | variable | That project's publishable (anon) key. Not secret. Without both, the build can't sign in |
| `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET`, `AZURE_TENANT_ID` | secrets | Windows signing: the Entra app registration |
| `AZURE_SIGNING_ENDPOINT` | variable | Windows signing: like `https://eus.codesigning.azure.net` |
| `AZURE_SIGNING_ACCOUNT` | variable | Windows signing: the Artifact Signing account name |
| `AZURE_SIGNING_PROFILE` | variable | Windows signing: the certificate profile name |

## macOS: Developer ID signing and notarization

To open without a warning, a Mac app downloaded from the web needs two things. It must be **signed** with a *Developer ID Application* certificate, and **notarized** (scanned by Apple, with a ticket stapled to the app).

Without them, the workflow gives the app an ad-hoc signature. Apple silicon Macs then say tab "can't be opened because Apple cannot check it for malicious software" instead of calling it damaged. People can still open it from System Settings → Privacy & Security → **Open Anyway**. That's fine for testing, but not for anyone else.

### 1. Join the Apple Developer Program

This costs $99 a year, at [developer.apple.com/programs/enroll](https://developer.apple.com/programs/enroll/).
- Enrolling as an **individual** puts your name on the certificate and in Gatekeeper.
- Enrolling as an **organization** shows the company name. It needs a D-U-N-S number and takes longer.

Only the **Account Holder** can create Developer ID certificates.

### 2. Create the Developer ID Application certificate

You need a Mac for this.

1. Open **Keychain Access** → menu **Keychain Access → Certificate Assistant → Request a Certificate From a Certificate Authority…**. Enter your email and name, leave CA email empty, choose **Saved to disk**, and save the `.certSigningRequest`.
2. Go to [developer.apple.com/account/resources/certificates](https://developer.apple.com/account/resources/certificates/list) → **+**. Choose **Developer ID Application** (not "Developer ID Installer", which is for `.pkg` files, and not "Apple Distribution", which is for the App Store). Pick the **G2 Sub-CA** profile type and upload the request.
3. Download the `.cer` and double-click it. It joins your login keychain next to the private key from step 1.

(Shortcut: Xcode → Settings → Accounts → your team → **Manage Certificates…** → **+** → *Developer ID Application* does steps 1 to 3 for you.)

### 3. Export it for GitHub

1. In Keychain Access → **login** → **My Certificates**, find **Developer ID Application: Your Name (TEAMID)**. Expand it to check that a private key is under it. Without one, the export is useless, so redo step 2 on this Mac.
2. Right-click the certificate (not the key) → **Export…** → format **.p12**, and set a strong password.
3. Copy it as base64 and save it as `APPLE_CERTIFICATE`:
   ```sh
   base64 -i ~/Desktop/tab-developer-id.p12 | pbcopy
   ```
4. Save the password as `APPLE_CERTIFICATE_PASSWORD`.
5. Get the identity name, and save it as `APPLE_SIGNING_IDENTITY`:
   ```sh
   security find-identity -v -p codesigning
   #  1) 3F2A… "Developer ID Application: Vincent Costa (AB12CD34EF)"
   ```
   The part in brackets is your **Team ID**.

Delete the `.p12` from your Desktop afterwards. Back up the certificate somewhere safe, like a password manager, because you can only have a few Developer ID certificates.

### 4. Notarization: an App Store Connect API key (recommended)

1. Go to [App Store Connect](https://appstoreconnect.apple.com/) → **Users and Access** → **Integrations** → **App Store Connect API** → **Team Keys**. The first time, the Account Holder has to click **Request Access** and accept the terms.
2. Click **Generate API Key** (or **+**). Name it `tab notarization` and give it the **Developer** access role, which is enough to notarize.
3. **Download** the `AuthKey_XXXXXXXXXX.p8`. Apple only lets you download it once.
4. Save these secrets:
   - `APPLE_API_KEY_P8`: the file's whole contents, including the `-----BEGIN PRIVATE KEY-----` lines (`pbcopy < AuthKey_XXXXXXXXXX.p8`).
   - `APPLE_API_KEY_ID`: the **Key ID** column.
   - `APPLE_API_ISSUER`: the **Issuer ID** shown above the key list.

**Alternative: an app-specific password.** Create one at [account.apple.com](https://account.apple.com/) → Sign-In and Security → **App-Specific Passwords**. Then set `APPLE_ID` (your Apple ID email), `APPLE_PASSWORD` (the app-specific password) and `APPLE_TEAM_ID`. The API key is better because it isn't tied to your personal login and you can revoke it on its own.

When signing and notarization are both set, the run summary says "Signed with Developer ID" and "Notarized…". Check a downloaded DMG with:
```sh
spctl -a -vvv -t install tab_1.2.3_aarch64.dmg   # "accepted, source=Notarized Developer ID"
```

### A note on the bundle identifier

The app's identifier is `dev.tab.app`. Tauri warns about identifiers ending in `.app`, because it clashes with the macOS bundle extension. It isn't a blocker for Developer ID. But the identifier also names the app's data folder, and an iOS App ID would use it too. If you want to change it (say to `dev.tab.desktop`), do it before the first public release, together with `src-tauri/Info.plist`, and plan to move existing data.

## Windows: SmartScreen and Defender

**Can it ship without setup and without warnings? No.**

- Windows Defender's antivirus doesn't usually flag a Tauri app.
- **SmartScreen** shows "Windows protected your PC" for any download it doesn't recognize. People have to click **More info → Run anyway**.
- An unsigned installer always gets this. It also never builds a reputation, because the reputation is tied to the signing certificate.

**Signing alone doesn't stop the warning on day one either.** Since 2024, even EV certificates no longer get instant SmartScreen trust. A new certificate starts with no reputation, and the warning goes away as people download and run it without trouble. The point of signing is to start building that reputation, and to show "Verified publisher: Your Name" instead of "Unknown publisher".

Your options:

| Option | Cost | Notes |
|---|---|---|
| **Azure Artifact Signing** (formerly Trusted Signing). **Recommended**, already wired into the workflow | About $9.99/month (Basic, 5,000 signatures) | Individuals in the USA and Canada; organizations in the USA, Canada, the EU and the UK. Identity check takes a few business days. Signs from CI with no hardware token. |
| OV code signing certificate (Certum, SSL.com, Sectigo, …) | About $200 to $400 a year | Since 2023 the key must live on a hardware token or a cloud HSM, so CI needs the vendor's cloud signing service. Use this if Artifact Signing isn't available where you live. |
| EV certificate | $400+ a year | No longer worth it just for SmartScreen. |
| Microsoft Store (MSIX) | Free | Microsoft re-signs MSIX packages, so there's no SmartScreen prompt. Tauri doesn't build MSIX today. Consider it later. |

### Setting up Azure Artifact Signing

1. You need an Azure account with a subscription (pay as you go is fine). In the portal, create an **Artifact Signing Account**. Note its **region endpoint**, like `https://eus.codesigning.azure.net` for East US.
2. In the account → **Identity validations** → **New** → **Public**, choose **Individual** (or Organization) and complete the check. Wait for it to say *Completed*.
3. **Certificate profiles** → **Create** → **Public Trust**, using that identity. Note the profile name.
4. Go to **Microsoft Entra ID** → **App registrations** → **New registration**, named `tab-release-signing`. Note the **Application (client) ID** and **Directory (tenant) ID**. Then **Certificates & secrets** → **New client secret**, and copy the value now.
5. Back in the Artifact Signing account → **Access control (IAM)** → **Add role assignment**. Pick **Artifact Signing Certificate Profile Signer** (it may still be called *Trusted Signing Certificate Profile Signer*) and assign it to `tab-release-signing`.
6. Save `AZURE_CLIENT_ID`, `AZURE_CLIENT_SECRET` and `AZURE_TENANT_ID` as **secrets**, and `AZURE_SIGNING_ENDPOINT`, `AZURE_SIGNING_ACCOUNT` and `AZURE_SIGNING_PROFILE` as **variables**.

The Windows job then installs [`artifact-signing-cli`](https://github.com/Levminer/artifact-signing-cli) and signs the app and both installers through Tauri's `signCommand`, as [Tauri's guide](https://v2.tauri.app/distribute/sign/windows/) describes. Client secrets expire (24 months at most), so put a reminder in your calendar.

## In-app updates

tab checks `https://github.com/VincentCosta6/tab/releases/latest/download/latest.json` when it starts and every hour after that. When a newer release is out, a card appears at the bottom of the nav and under Settings → Updates; one click downloads it, checks its signature, installs it and restarts tab. On Windows the installer runs in passive mode (a progress bar, no questions).

Tauri's updater refuses an update unless it's signed with your updater key. This is separate from Apple and Microsoft signing, and it's free. Once `TAURI_SIGNING_PRIVATE_KEY` is set, every release also uploads the update bundles (`.app.tar.gz` and `.sig` for macOS, `.exe.sig` for Windows) and a `latest.json` that lists them.

1. Generate the key once, on your own machine:
   ```sh
   pnpm tauri signer generate -w ~/.tauri/tab-updater.key
   ```
   Pick a password. The output shows the **public key** (also saved as `~/.tauri/tab-updater.key.pub`).
2. Save the **private** key file's contents as the secret `TAURI_SIGNING_PRIVATE_KEY`, and the password as `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`.
3. Save the **public** key (the `.pub` file's contents, one line) as the **variable** `TAURI_UPDATER_PUBKEY`. It isn't secret; it goes into every build so the app can check what it downloads.
4. Back up the private key. **If you lose it, installed copies can never update again.** You'd have to ship a new key in a release people install by hand.

Both halves are needed. A build made without `TAURI_UPDATER_PUBKEY` can't update itself and says so in Settings → Updates, so the first release with the key is one people install by hand; every release after it arrives on its own. Development builds never update themselves.

Updates only come from the release marked latest. Releases cut with **Run workflow** are published only after every build is uploaded, so the app never sees a half-finished one. A release published from the GitHub UI is latest while it builds, and checks during that time report "Couldn't check" until its `latest.json` is uploaded.

## iOS (later)

The workflow has an `ios` job that's switched off. tab's desktop engine (agents, the local hub, the sandbox) can't run on iOS, so the phone app will be a client of the hub, not this app recompiled. When that exists, you'll need the following in the same Apple Developer account:

1. **App ID**: Certificates, IDs & Profiles → **Identifiers** → **+** → App IDs → App, with an explicit bundle ID (see the bundle identifier note above) and any capabilities it uses, like push notifications.
2. **Apple Distribution certificate**: Certificates → **+** → **Apple Distribution**, using the same CSR steps as above. Export it as a `.p12`.
3. **Provisioning profile**: Profiles → **+** → **App Store Connect** (Distribution). Pick the App ID and the Apple Distribution certificate, then download the `.mobileprovision`.
4. **The app in App Store Connect**: Apps → **+** → New App, with that bundle ID.
5. **Secrets**: `IOS_CERTIFICATE` (the `.p12`, base64), `IOS_CERTIFICATE_PASSWORD`, and `IOS_MOBILE_PROVISION` (the profile, base64). Tauri's iOS build reads these. Uploading to TestFlight can reuse the App Store Connect API key from the macOS steps.

Then run `pnpm tauri ios init`, commit `src-tauri/gen/apple`, and turn the job on.
