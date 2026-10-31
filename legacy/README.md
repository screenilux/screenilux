# The original Python build

This is the version Screenilux started as: a single-file Tkinter app that
recorded screens with `mss`, composited overlays with OpenCV, and piped frames
to ffmpeg. It works, and it is kept here because it is what the Rust port was
verified against — the session manifest format, the frame pacing, the
encoder-fallback behaviour and the mouse effects were all specified by this
code before being rewritten.

It is no longer developed. To run it:

```
pip install -r requirements.txt
python screenrec.py
```

It needs Python 3.9+, `mss`, `numpy`, `opencv-python-headless`, and ffmpeg on
PATH. The Rust build needs none of those.

Several bugs found while porting were left unfixed here deliberately, so the
two can still be compared. They are listed in the port notes in the git
history — the main ones being a stale shared clock after a failed screen, a
stream-copy join across mixed encoders, and a mouse trail that never fades
while the pointer is stationary.
