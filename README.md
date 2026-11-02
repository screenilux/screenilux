<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.svg">
    <img src="docs/logo.svg" alt="Screenilux logo: two screens joined by a recording dot" width="112">
  </picture>
</p>

<h1 align="center">Screenilux</h1>

<p align="center">
  <strong>Record every screen at once. Play them back together. Copy any text you saw.</strong>
</p>

<p align="center">
  <a href="https://github.com/screenilux/screenilux/actions/workflows/ci.yml"><img src="https://github.com/screenilux/screenilux/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI/CD"></a>
  <a href="https://github.com/screenilux/screenilux/releases/latest"><img src="https://img.shields.io/badge/download-latest-04736f" alt="Download the latest release"></a>
  <img src="https://img.shields.io/badge/platform-Windows%20%7C%20Linux-012625" alt="Windows and Linux">
  <a href="LICENSE"><img src="https://img.shields.io/badge/licence-GPL--3.0-069a95" alt="Licence: GPL-3.0"></a>
  <img src="https://img.shields.io/badge/links-end--to--end%20encrypted-07beb8" alt="Share links are end-to-end encrypted">
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/made%20with-Rust-dea584?logo=rust&amp;logoColor=white" alt="Made with Rust"></a>
</p>

<p align="center">
  <a href="#download">Download</a> ·
  <a href="#recording">Recording</a> ·
  <a href="#playing">Playing</a> ·
  <a href="#how-it-compares">How it compares</a> ·
  <a href="#building-it-yourself">Build it</a>
</p>

---

Screenilux is the screen recorder for people who work across more than one
screen.

- **Every screen, in sync.** It records all your monitors at the same time,
  each as its own sharp video, and plays them back in step, following your
  pointer from screen to screen. Other recorders capture one screen, or
  squeeze several into one wide, blurry video.
- **Copy any text you saw.** Pause anywhere and the text on screen, such as
  code, commands, error messages or numbers, becomes something you can
  select and copy. Search a recording for a word and jump to where it
  appeared.
- **Links only your recipients can watch.** Share a recording and it is
  encrypted on your computer first. The key is in the link, so neither
  Screenilux nor whoever stores it can watch, and the people you send it to
  can still play every screen and search and copy the text.

It is free and open source for Windows and Linux, with no account and no
limits on length.

<p align="center">
  <img src="docs/play.png" alt="A two-screen recording playing side by side, with a search for a word on the right" width="900">
</p>

## Why Screenilux

| | |
|---|---|
| **Every screen, in sync** | Most recorders capture one screen, or squeeze several into one wide, blurry video. Screenilux records each screen at its full size and keeps them in step, so playback can follow the pointer from one to the next. |
| **Text you can copy and search** | Code, commands, error messages, numbers: pause and drag across them to copy, or search the whole recording for a word. It reads the screen on your computer, offline. |
| **Private links** | Shared recordings are encrypted before they leave your computer, with the key in the link. Only the people you send it to can watch. |
| **Yours, on your computer** | No sign-up and no limits on length or number of recordings. Files are standard MP4s you can keep, move and open anywhere. |
| **Stays out of the picture** | While you record, the window shrinks to a small bar that never shows up in the video (on Windows). |

## Recording

<p align="center">
  <img src="docs/record.png" alt="Screenilux, ready to record two screens with the camera and microphone on" width="360">
</p>

- **One screen or several at once.** Live previews show what each screen is
  showing; tick the ones to record.
- **Your voice and your computer's sound.** Record the microphone, what your
  computer plays (videos, calls, apps), or both mixed together.
- **Camera bubble.** Add your camera as a round bubble you can drag anywhere,
  even while recording.
- **Pointer effects.** Show the pointer, zoom in with `Ctrl+Alt` and the mouse
  wheel, and hear a soft click each time you click.
- **Pause whenever you like.** The paused time is left out of the video.
- **New screens are found by themselves.** Plug a screen in or unplug one and
  the list updates.
- **A link the moment you stop.** Turn on sharing and the link is on your
  clipboard as soon as you stop. See [Sharing a link](#sharing-a-link).

## Playing

- **Every screen plays together,** in a window of its own: one at a time
  following the pointer, side by side, or stacked.
- **Copy text straight from the video.** Pause, then drag across any words to
  select and copy them.
- **Search inside recordings.** Choose *Make text searchable*, then search for
  a word and jump to each place it appeared.
- **Full screen.** Press `F` or double-click the picture: the recording fills
  the screen and the controls float over it, out of the way while it plays.
  `Esc` brings the window back.
- **Open recordings your way:** double-click one, drop it on the window, or
  pick one under **Recordings**.

<p align="center">
  <img src="docs/full-screen.png" alt="Full screen, with the controls floating over a two-screen recording" width="900">
</p>

## Sharing a link

Turn on **Share a link when you stop** in **Settings** (the gear). When you
stop recording, Screenilux uploads the recording and copies a link to it.
Anyone you send it to can open it in a browser and watch every screen play in
step, with no account and nothing to install. If the recording has been made
searchable, they can search it and copy its text too; making it searchable
after sharing adds the text to the same link.

You can send the link straight away. Until the upload finishes, the page
shows how far along it is, then starts playing by itself.

**Only people with the link can watch.** Recordings are encrypted on your
computer before they are uploaded, and the key is part of the link. Neither
Screenilux nor your storage provider can watch them.

Choose where recordings go:

- **Screenilux.** Nothing to set up. Links last 7 days, and each computer
  can have up to 1 GB shared at a time.
- **My bucket.** Your own storage: Cloudflare R2, Amazon S3, Backblaze B2,
  Wasabi or any other S3-compatible service. Recordings stay until you delete
  them, and your keys never leave your computer.
  [Setting up your own storage](docs/own-storage.md) takes about five minutes.

Recordings still open only in the Screenilux app on your computer; the
browser page is for the people you send the link to. In Settings you can
also choose not to keep a copy on your computer once a recording is
uploaded.

## How it compares

Tools like Loom are built for sending a quick talking-head video to a
colleague. Screenilux is built for the work itself: everything across your
screens, readable and searchable afterwards, and private when shared.

| | Screenilux | Loom |
|---|---|---|
| Several screens at once | Yes, played back together in step | One screen per recording |
| Text on screen | Copy it from a paused frame, search a recording for it | Not readable; transcripts cover what was said |
| Shared links | Encrypted on your computer; only people with the link can watch | Kept on Loom's servers, protected by link settings and passwords |
| Where recordings are kept | On your computer, and in your own storage if you like | On Loom's servers |
| Length and number | No limits | 5 minutes and 25 recordings on the free plan |
| Account | None | Required |
| Price | Free and open source | Free plan with limits, or a monthly fee per person |

Screenilux does not try to be a video messaging service. Comments,
reactions, view analytics and AI summaries are what Loom and similar tools
do well, and are not on its list. What is on it: more of the three things
above, on more computers.

<sub>Loom's details are from its help pages and plan listings in November 2026.</sub>

## Download

Get the latest version from
[Releases](https://github.com/screenilux/screenilux/releases/latest).

**Windows 10 and 11**

1. Download the Windows zip and unzip it.
2. Run `screenilux.exe`. There is nothing to install and no administrator
   rights are needed.

The app is not signed yet, so Windows may say it protected your PC the first
time. Choose **More info**, then **Run anyway**.

**Linux**

1. Install GStreamer for video and sound, and Tesseract for reading text. On
   Ubuntu or Debian:

   ```
   sudo apt install gstreamer1.0-plugins-good gstreamer1.0-plugins-bad gstreamer1.0-plugins-ugly gstreamer1.0-libav tesseract-ocr
   ```

2. Download the Linux file, unpack it and run `screenilux`.

Screenilux on Linux records an X11 session. On Ubuntu, choose
**Ubuntu on Xorg** on the login screen. Wayland sessions are not supported yet.

Recordings are saved in `Videos/ScreenRec` in your home folder. A single
screen is saved as an MP4 that plays anywhere. Several screens are saved
together as one `.screenilux` file, which opens in Screenilux.

## Good to know

- Zooming in makes the picture slightly softer.
- Copying text needs a Windows language with text recognition installed, or
  Tesseract on Linux. The app tells you if it is missing and how to add it.
- Text shown for less than two seconds may not turn up in search, but you can
  still pause on it and copy it.
- Long multi-screen recordings take a moment to open.
- Pointer effects do not work over apps running as administrator.
- On Linux, the recording bar and the camera preview show in the recording,
  because X11 has no way to hide a window from capture. Turn off **Shrink to a
  bar while recording** to keep the bar out of the way.

## Building it yourself

Install [Rust](https://rustup.rs). On Linux, also install GStreamer's
development files (`libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev` on
Ubuntu). Then run:

```
cargo run --release -p screenilux
```

Ideas and fixes are welcome: open an
[issue](https://github.com/screenilux/screenilux/issues) or a pull request.
See [Contributing](CONTRIBUTING.md) for how, including the one-comment
[Contributor Licence Agreement](CLA.md).

## Licence

[GPL-3.0](LICENSE). You are free to use, change and sell Screenilux. If you
share a changed version, you must share its source code under the same
licence.
