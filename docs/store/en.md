# Store listing draft (English)

Drafts for Google Play and the App Store. Every claim matches what the phone
app does today (see [mobile-prototype.md](../mobile-prototype.md) and
[architecture-v2.md](../architecture-v2.md)); the app has not yet been run on a
device, so check each line against a real build before submitting. Nothing
here promises a hosted service, team vaults on the phone, remote forwards or
folder transfers, because those are not on the phone.

## Name

AnixOps SSH

## Short description (Google Play, 80 characters at most)

SSH terminal, files and port forwards, with an encrypted vault on your phone.

(76 characters)

## Subtitle (App Store, 30 characters at most)

SSH terminal and files

## Full description (4000 characters at most)

AnixOps SSH is an open-source SSH client for your phone. Your hosts, keys and
passwords stay on the device, encrypted, and nothing is sent anywhere unless
you set up sync with a server you run yourself.

TERMINAL
- A full terminal with a key bar for Esc, Tab, Ctrl, arrows and the other keys
  a soft keyboard lacks, and a bar you can customise.
- Works with the on-screen keyboard and input methods, including Chinese.
- Select text by touch with handles, copy and paste, scroll with one or two
  fingers, pinch to change the font size.
- Hardware keyboard shortcuts on tablets.
- Attach to tmux or screen and reconnect automatically when the network drops
  or the app comes back from the background.

FILES
- Browse a server over SFTP, make folders, rename and delete (with a
  confirmation), upload and download files.

PORT FORWARDS
- Local forwards (ssh -L) with a switch per forward and the reason when one
  fails.

HOSTS
- Save hosts with groups, tags and favorites; search by name, address, group or
  #tag.
- Reach a host through another saved host (jump host).
- A terminal, the file browser and a forward on the same host share one
  connection, so you sign in once.
- Host keys are checked and you are asked before trusting a new one.

VAULT AND PRIVACY
- Saved passwords, private keys and key passphrases are encrypted on the
  device. The key is held by the Android Keystore or the iOS Keychain.
- Optional vault passphrase, and unlock with fingerprint or face that cannot be
  released without the sensor. The passphrase always remains as a fallback.
- The vault locks when the app goes to the background.
- Crash reports are asked for on first start and anonymous usage statistics are
  off until you turn them on. Terminal content, host names and addresses are
  never collected.

SYNC
- Optionally sync hosts across your devices through the open-source
  AnixOps SSH server that you or your team host. Data is end-to-end encrypted:
  the server cannot read it.

OPEN SOURCE
- The app, the sync server and the protocol are open source. The source is at
  https://github.com/AnixOps/EasySSH

## Keywords (App Store, 100 characters at most, comma separated)

ssh,terminal,sftp,server,devops,sysadmin,tunnel,port forward,tmux,vault,remote,shell

(84 characters)

## Google Play category and tags

Category: Tools (or Developer Tools if offered). Tags: SSH client, terminal emulator.

## What's new (first release)

First release for phones: SSH terminal, SFTP files, local port forwards, jump
hosts, an encrypted vault with biometric unlock, and optional end-to-end
encrypted sync with your own server.

## Notes for the review forms

- Encryption: the app uses standard encryption (SSH, TLS, and authenticated
  encryption for the vault). Answer the export-compliance question accordingly.
- Data safety / privacy: no data leaves the device by default. Optional,
  user-controlled: crash reports and anonymous usage statistics
  ([telemetry.md](../telemetry.md)), and sync with the user's own server.
- Permissions: network; biometrics (Face ID usage text is in the iOS project);
  an optional foreground notification on Android (off by default) to keep a
  session alive in the background.
- App access for reviewers: the app needs an SSH server. Provide a test host
  and account in the review notes.
