# Privacy policy

*Applies to the audiomon Android app and the audiomon Windows app. Last updated: 25 October 2026.*

**audiomon does not collect, store or share any personal data.**

## What the apps do with data

- The Windows app captures the sound your PC is playing and sends it **directly to your own phone**, over the USB cable (USB tethering) or your local Wi-Fi network. The phone app plays it.
- That audio, and the small control messages that keep the two devices in sync, travel **only between your PC and your phone**. They are never sent to the internet, to us, or to anyone else, and they are not recorded.
- The apps contain **no analytics, no advertising, no tracking and no accounts**.
- The Windows app keeps its settings and a small diagnostic log on your PC (`%LOCALAPPDATA%\audiomon`); they never leave your computer.

## Android permissions and why they are needed

| Permission | Why |
|---|---|
| Network access | To receive audio from your PC over USB tethering or your local Wi-Fi. The app only talks to devices on your local network. |
| Foreground service (media playback) | To keep playing when the screen is off or another app is open. A notification is shown while it runs. |
| Notifications | To show that notification. |
| Keep awake (wake lock) | So playback does not stop when the phone goes to sleep while streaming. |
| USB accessory (optional) | Only for the advanced direct-USB mode, when you connect the phone to your PC that way. |

## Children

audiomon is not directed at children and collects no data from anyone.

## Changes and contact

If this policy changes, the new version will be published here. Questions: open an issue at https://github.com/dxcot/audiomon/issues.
