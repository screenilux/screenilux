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
  <img src="https://img.shields.io/badge/free-no%20account%2C%20no%20limits-07beb8" alt="Free, with no account and no limits">
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

Screenilux is a free screen recorder and player for Windows and Linux, made
for work that spreads across more than one screen.

It records **all your screens at the same time**, each as its own sharp video,
and plays them back **in step**, so you can watch the work move from monitor
to monitor exactly as it happened. Pause anywhere and the **text on screen
becomes something you can select and copy**. Make a recording searchable and
jump straight to the moment a word appeared.

Everything stays **on your computer**: no account, no upload, no time limit,
no watermark.

<p align="center">
  <img src="docs/play.png" alt="A two-screen recording playing side by side, with a search for a word on the right" width="900">
</p>

## Why Screenilux

| | |
|---|---|
| **Every screen, in sync** | Most recorders capture one screen, or squeeze several into one wide, blurry video. Screenilux records each screen at its full size and keeps them in step, so playback can follow the pointer from one to the next. |
| **Text you can copy and search** | Code, commands, error messages, numbers: pause and drag across them to copy, or search the whole recording for a word. It reads the screen on your computer, offline. |
| **Yours, on your computer** | No sign-up, no cloud, no limits on length or number of recordings. Files are standard MP4s you can keep, move and open anywhere. |
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

- **Every screen plays together:** one at a time following the pointer, side
  by side, or stacked.
- **Copy text straight from the video.** Pause, then drag across any words to
  select and copy them.
- **Search inside recordings.** Choose *Make text searchable*, then search for
  a word and jump to each place it appeared.
- **Full screen.** Press `F` or double-click the picture: the recording fills
  the screen and the controls float over it, out of the way while it plays.
  `Esc` brings the window back.
- **Open recordings your way:** double-click one, drop it on the window, or
  use *Open recording*.

<p align="center">
  <img src="docs/full-screen.png" alt="Full screen, with the controls floating over a two-screen recording" width="900">
</p>

## Sharing a link

Turn on **Share a link when you stop** under **More options**. When you stop
recording, Screenilux uploads the recording and copies a link to it. Anyone
can open the link in a browser and watch every screen play in step, with no
account and nothing to install.

You can send the link straight away. Until the upload finishes, the page
shows how far along it is, then starts playing by itself.

**Only people with the link can watch.** Recordings are encrypted on your
computer before they are uploaded, and the key is part of the link. Neither
Screenilux nor your storage provider can watch them.

Choose where recordings go:

- **Screenilux.** Nothing to set up. Links last 7 days, for recordings up
  to 4 GB.
- **My bucket.** Your own storage: Cloudflare R2, Amazon S3, Backblaze B2,
  Wasabi or any other S3-compatible service. Recordings stay until you delete
  them, and your keys never leave your computer.
  [Setting up your own storage](docs/own-storage.md) takes about five minutes.

Recordings still open only in the Screenilux app on your computer; the
browser page is for the people you send the link to.

## How it compares

Tools like Loom are built for sending a quick video as a link. Screenilux is
built for recording everything you are working on, on your own computer, and
getting the most out of it afterwards.

| | Screenilux | Loom |
|---|---|---|
| Several screens at once | Yes, played back together in step | No, one screen per recording |
| Length of a recording | No limit | 5 minutes on the free plan |
| Number of recordings | No limit | 25 per person on the free plan |
| Account | None | Required |
| Share a link | Yes, from Screenilux or your own storage | Yes |
| Where recordings are kept | On your computer. Shared ones also in your own storage, or on Screenilux for 7 days | On Loom's servers |
| Price | Free and open source | Free plan with limits, or a monthly fee per person |

Screenilux also reads the text on screen, on your computer and without
internet, so you can copy it from a paused video or search a recording for a
word.

Loom is ahead in comments, reactions, editing, and apps for Mac and phones.

<sub>Loom's limits are from its help pages and plan listings in November 2026.</sub>

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
