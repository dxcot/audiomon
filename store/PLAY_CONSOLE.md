# Publishing audiomon on Google Play

Everything the Play Console asks for, in order. Files referenced here live in this `store/` folder; the app bundle is built with `cd android && .\gradlew.bat bundleRelease` (output: `android/app/build/outputs/bundle/release/app-release.aab`, signed with the release key).

Google's rules change; check the Console's current requirements as you go.

## 1. Developer account (once)

1. Go to https://play.google.com/console and sign in with the Google account that will own the app.
2. Choose a **personal** account, pay the one-time registration fee (US$25) and complete **identity verification** (government ID).
3. The developer name you choose is shown publicly on the store page.
4. You also need a **contact email** for the store listing (shown publicly). A dedicated address is a good idea.

## 2. Create the app

*Home → Create app*

| Field | Answer |
|---|---|
| App name | `audiomon: PC speaker` |
| Default language | English (United States) |
| App or game | App |
| Free or paid | Free |
| Declarations | Accept |

## 3. App content (Policy → App content)

| Section | Answer |
|---|---|
| Privacy policy | `https://github.com/dxcot/audiomon/blob/main/PRIVACY.md` |
| App access | *All or some functionality is restricted* → add instructions: "audiomon plays audio streamed from the free audiomon Windows app (https://github.com/dxcot/audiomon/releases). Install it on a Windows PC, open audiomon on the phone, and keep both on the same Wi-Fi. No login is needed." |
| Ads | No, the app does not contain ads |
| Content rating | Start the questionnaire, category **"All other app types"**; answer **No** to every content question (no violence, sexuality, language, controlled substances, gambling, user interaction or sharing, location). Expected result: Everyone / PEGI 3 |
| Target audience | **13–15, 16–17 and 18+** (do not select under 13, which would bring in the Families policy). Not designed for children |
| News app | No |
| Data safety | Does the app collect or share user data? **No**. (Audio goes directly from the user's own PC to the phone on their local network or cable; nothing is collected, stored or sent to the developer or third parties.) |
| Government app | No |
| Financial features | None |
| Health | No |
| Foreground service permissions | Declare **Media playback**. Description: "Plays audio streamed from the user's own Windows PC while the screen is off or another app is in front. The user starts and stops it in the app, and a notification is shown while it plays." Google may ask for a short video showing this (screen recording: open the app, play sound on the PC, lock the phone and show the notification) |

## 4. Store listing (Grow → Store presence → Main store listing)

- Text: `listing-en.md` (default) and, under *Translations*, Spanish from `listing-es.md`
- App icon: `graphics/icon-512.png`
- Feature graphic: `graphics/feature-en.png` (and `feature-es.png` for the Spanish translation)
- Phone screenshots: `screenshots/en/*.png` and `screenshots/es/*.png` (at least 2 per language)
- Category: **Music & Audio**
- Contact details: your contact email; website `https://github.com/dxcot/audiomon`

## 5. Closed test (required for new personal accounts)

New personal developer accounts must run a **closed test with at least 12 testers, opted in for 14 days in a row**, before they can apply for production access.

1. *Test and release → Testing → Closed testing → Create track*.
2. Upload `app-release.aab`. On the first upload, accept **Play App Signing**: Google then signs the app for the store, and the audiomon release key becomes the **upload key**. (Keep it safe: it is what proves future uploads are yours.)
3. Release notes: "First release: use your phone as your PC's speaker over USB tethering or Wi-Fi."
4. Countries: all, or the ones you want.
5. Testers: an email list or a Google Group with at least 12 people. Share the opt-in link with them; they must stay opted in for 14 days and should actually use the app.
6. Send the release for review.

## 6. Production

After the 14 days, *Dashboard → Apply for production* answers a few questions about the test. Once approved, create a **Production** release with the same (or a newer) bundle.

## Updating later

Raise `versionCode` (and `versionName`) in `android/app/build.gradle.kts`, run `bundleRelease`, and upload the new `.aab` to the track you want.
