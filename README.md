# Screenilux

Record your screens on Windows, then play them back and copy any text you see
in them.

![Screenilux, ready to record two screens](docs/record.png)

## Recording

- **One screen or several at once.** Each screen gets its own video, and they
  all stay in step.
- **Pause whenever you like.** The paused time is left out of the video.
- **Out of your way.** While you record, the window shrinks to a small bar at
  the bottom of the screen. The bar never appears in the recording.
- **Pointer effects.** Show the pointer, zoom in with `Ctrl+Alt` and the mouse
  wheel, and hear a soft click each time you click.
- **Camera and microphone.** Add your camera as a bubble you can drag
  anywhere, even while recording.
- **New screens are found by themselves.** Plug a screen in or unplug one and
  the list updates.

## Playing

![A two-screen recording playing side by side, with a title selected](docs/play.png)

- **Every screen plays together:** one at a time following the pointer, side
  by side, or stacked.
- **Copy text straight from the video.** Pause, then drag across any words to
  select and copy them.
- **Search inside recordings.** Choose *Make text searchable*, then search for
  a word and jump to each place it appeared.
- **Open recordings your way:** double-click one, drop it on the window, or
  use *Open*.

## Getting it

1. Download the latest zip from
   [Releases](https://github.com/EagleMind/screenilux/releases/latest).
2. Unzip it and run `screenilux.exe`. There is nothing to install and no
   administrator rights are needed.

The app is not signed yet, so Windows may say it protected your PC the first
time. Choose **More info**, then **Run anyway**.

Recordings are saved in `Videos\ScreenRec`. A single screen is saved as an MP4
that plays anywhere. Several screens are saved together as one `.screenilux`
file, which opens in Screenilux.

## Good to know

- Sound comes from your microphone only. Sounds your computer plays are not
  recorded.
- Zooming in makes the picture slightly softer.
- Copying text needs a Windows language with text recognition installed. The
  app tells you if it is missing and where to add it.
- Text shown for less than two seconds may not turn up in search, but you can
  still pause on it and copy it.
- Long multi-screen recordings take a moment to open.
- Pointer effects do not work over apps running as administrator.

## Building it yourself

Install [Rust](https://rustup.rs), then run:

```
cargo run --release -p screenilux
```

## Licence

GPL-3.0. You are free to use, change and sell Screenilux. If you share a
changed version, you must share its source code under the same licence.
